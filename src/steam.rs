//! Publishes status to Steam as the "in-game" text.
//!
//! Two modes:
//! - [`set_once`]: one-shot, used by the `steam-set` debug command.
//! - [`Publisher`]: a long-lived background task holding the CM connection,
//!   driven by the `run` loop.
//!
//! Interactive login is driven through `steam-auth-rs` directly (rather than
//! `SteamClient::log_on_with_password`) because that helper hardcodes a guard
//! priority of `EmailCode > DeviceCode > DeviceConfirmation`. On accounts with
//! a mobile authenticator Steam offers *both* `DeviceConfirmation` and
//! `DeviceCode`; picking the code path means that if you approve the push
//! first, the later code submission fails with `DuplicateRequest`. We prefer
//! the push.
//!
//! The connection is kept alive by the background task calling
//! `poll_event_timeout`, which also sends heartbeats and drives reconnection.

use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::{Context, Result};
use steam_auth::{CredentialsDetails, EAuthSessionGuardType, EAuthTokenPlatformType, LoginSession};
use steam_client::{
    AuthEvent, ConnectionEvent, EPersonaState, LogOnDetails, SteamClient, SteamEvent, SteamOptions,
    TokenStore,
};
use tokio::{sync::mpsc, task::JoinHandle};

const SESSION_FILE: &str = "steam_session.json";

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Interactive login. Returns `(account_name, refresh_token)`.
async fn interactive_login() -> Result<(String, String)> {
    let account = prompt("Steam 用户名: ")?;
    println!("（密码会明文回显，仅本地使用）");
    let password = prompt("Steam 密码: ")?;

    let mut session = LoginSession::new(
        EAuthTokenPlatformType::KEAuthTokenPlatformTypeSteamClient,
        None,
    );

    let start = session
        .start_with_credentials(CredentialsDetails {
            account_name: account,
            password,
            persistence: None,
            steam_guard_machine_token: None,
            steam_guard_code: None,
        })
        .await
        .context("start_with_credentials failed（检查网络/代理）")?;

    if start.action_required {
        let actions = start.valid_actions.clone().unwrap_or_default();
        let has = |t: EAuthSessionGuardType| actions.iter().any(|a| a.guard_type == t);

        if has(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceConfirmation) {
            println!("📱 请在 Steam 手机 App 上点击「批准」——批准后无需输入任何验证码");
        } else if has(EAuthSessionGuardType::KEAuthSessionGuardTypeEmailConfirmation) {
            println!("📧 请点击 Steam 发来的邮件里的确认链接");
        } else if has(EAuthSessionGuardType::KEAuthSessionGuardTypeEmailCode) {
            let code = prompt("邮箱验证码: ")?;
            session.submit_steam_guard_code(&code).await?;
        } else if has(EAuthSessionGuardType::KEAuthSessionGuardTypeDeviceCode) {
            println!("请在 Steam 手机 App 的 Steam Guard 页面查看 5 位验证码");
            let code = prompt("验证码: ")?;
            session.submit_steam_guard_code(&code).await?;
        } else {
            println!("⚠️ 未知的验证类型: {actions:?}");
            let code = prompt("Steam Guard 验证码: ")?;
            session.submit_steam_guard_code(&code).await?;
        }
    }

    let interval = Duration::from_secs_f32(session.poll_interval().max(1.0));
    loop {
        match session.poll().await.context("poll failed")? {
            Some(result) => {
                println!("✅ 登录成功: {}", result.account_name);
                return Ok((result.account_name, result.refresh_token));
            }
            None => tokio::time::sleep(interval).await,
        }
    }
}

async fn log_in(client: &mut SteamClient) -> Result<()> {
    let store = TokenStore::new(SESSION_FILE);

    if let Some(details) = store.load(None) {
        println!("→ 使用已保存的 refresh token 登录…");
        match client.log_on(details).await {
            Ok(r) => {
                println!("✅ 已登录 {}", r.steam_id.steam3());
                return Ok(());
            }
            Err(e) => println!("⚠️ token 失效（{e:?}），需要重新登录"),
        }
    }

    let (account_name, refresh_token) = interactive_login().await?;
    store
        .save(account_name.clone(), refresh_token.clone(), 0)
        .context("saving refresh token")?;
    println!("💾 refresh token 已保存（下次免密登录）");

    let r = client
        .log_on(LogOnDetails {
            account_name: Some(account_name),
            refresh_token: Some(refresh_token),
            ..Default::default()
        })
        .await?;
    println!("✅ 已连接 Steam CM ({})", r.steam_id.steam3());
    Ok(())
}

fn build_client() -> SteamClient {
    SteamClient::new(SteamOptions {
        machine_name: Some("steam-now-playing".to_string()),
        ..Default::default()
    })
}

/// One-shot: log in, publish `text`, hold it, then clear. For debugging.
pub async fn set_once(text: &str, hold_secs: u64) -> Result<()> {
    let mut client = build_client();
    log_in(&mut client).await?;
    client.set_persona(EPersonaState::Online, None).await?;

    println!("→ 设置为正在玩: {text:?}");
    client
        .games_played_with_extra(vec![], Some(text.to_string()))
        .await?;

    println!("⏳ 保持 {hold_secs}s —— 去看你的 Steam 个人资料 / 好友列表");
    tokio::time::sleep(Duration::from_secs(hold_secs)).await;

    client.games_played(vec![]).await?;
    println!("🧹 已清除状态");
    client.log_off().await?;
    Ok(())
}

enum Cmd {
    Set(Option<String>),
    Shutdown,
}

/// Handle to a background task that owns the Steam connection.
pub struct Publisher {
    tx: mpsc::Sender<Cmd>,
    handle: JoinHandle<()>,
}

impl Publisher {
    /// Log in and start the connection-pumping task.
    pub async fn spawn() -> Result<Self> {
        let mut client = build_client();
        log_in(&mut client).await?;
        client.set_persona(EPersonaState::Online, None).await?;

        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(task(client, rx));
        Ok(Self { tx, handle })
    }

    /// Set (or clear, with `None`) the in-game status. Errors if the Steam
    /// task has died (e.g. the connection dropped and could not recover).
    pub async fn set(&self, text: Option<String>) -> Result<()> {
        self.tx
            .send(Cmd::Set(text))
            .await
            .map_err(|_| anyhow::anyhow!("Steam 后台任务已退出"))
    }

    /// Clear the status, log off, and wait for the task to finish.
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.tx.send(Cmd::Shutdown).await;
        let _ = self.handle.await;
        Ok(())
    }
}

async fn task(mut client: SteamClient, mut rx: mpsc::Receiver<Cmd>) {
    let mut current: Option<String> = None;

    loop {
        tokio::select! {
            // Drives heartbeats + reconnection. 30s matches the default
            // heartbeat interval.
            event = client.poll_event_timeout(Duration::from_secs(30)) => {
                match event {
                    Ok(Some(SteamEvent::Connection(ConnectionEvent::Disconnected { reason, will_reconnect }))) => {
                        eprintln!("🔌 Steam 断开: {reason:?}（自动重连: {will_reconnect}）");
                        if !will_reconnect {
                            break;
                        }
                    }
                    Ok(Some(SteamEvent::Connection(ConnectionEvent::ReconnectAttempt { attempt, max_attempts, .. }))) => {
                        eprintln!("🔄 重连中 {attempt}/{max_attempts}");
                    }
                    Ok(Some(SteamEvent::Auth(AuthEvent::LoggedOff { result }))) => {
                        eprintln!("👋 已登出: {result:?}");
                        break;
                    }
                    Err(e) => {
                        eprintln!("⚠️ Steam 事件轮询错误: {e:?}");
                    }
                    _ => {}
                }
            }

            cmd = rx.recv() => match cmd {
                Some(Cmd::Set(text)) => {
                    if text == current {
                        continue;
                    }
                    let result = match &text {
                        Some(t) => client.games_played_with_extra(vec![], Some(t.clone())).await,
                        None => client.games_played(vec![]).await,
                    };
                    match result {
                        Ok(()) => current = text,
                        Err(e) => eprintln!("⚠️ 更新 Steam 状态失败: {e:?}"),
                    }
                }
                Some(Cmd::Shutdown) | None => {
                    let _ = client.games_played(vec![]).await;
                    let _ = client.log_off().await;
                    break;
                }
            },
        }
    }
}
