mod format;
mod gsmtc;
mod steam;

use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::time::MissedTickBehavior;

#[derive(Parser)]
#[command(
    name = "steam-now-playing",
    version,
    about = "把系统正在播放的内容镜像到 Steam 状态"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 列出当前所有媒体会话（用来找某个 App 的 AUMID）
    Probe,

    /// 常驻运行：轮询 Apple Music，把当前歌曲同步到 Steam 状态
    Run {
        /// 轮询间隔（秒）
        #[arg(long, default_value_t = 3)]
        poll: u64,
    },

    /// 调试用：登录并设置一条自定义状态，保持若干秒后清除
    SteamSet {
        /// 要显示的状态文字
        #[arg(default_value = "Apple Music · ♪ Test Song — Test Artist")]
        text: String,

        /// 保持多少秒
        #[arg(default_value_t = 15)]
        hold_secs: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Some(Command::Probe) => gsmtc::probe(),
        Some(Command::SteamSet { text, hold_secs }) => steam::set_once(&text, hold_secs).await,
        Some(Command::Run { poll }) => run(poll).await,
        None => run(3).await,
    }
}

async fn run(poll_secs: u64) -> Result<()> {
    println!("启动：每 {poll_secs}s 检查一次 Apple Music（Ctrl+C 退出）");

    let publisher = steam::Publisher::spawn().await?;

    let mut current: Option<String> = None;
    let mut ticker = tokio::time::interval(Duration::from_secs(poll_secs.max(1)));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n退出中，清除状态…");
                publisher.shutdown().await?;
                break;
            }
            _ = ticker.tick() => {
                let target = match gsmtc::current_apple_music() {
                    Ok(Some(t)) if !t.title.trim().is_empty() => Some(format::status_text(&t)),
                    Ok(_) => None,
                    Err(e) => {
                        eprintln!("⚠️ 读取 GSMTC 失败: {e:?}");
                        continue;
                    }
                };

                if target != current {
                    match &target {
                        Some(t) => println!("♪ {t}"),
                        None => println!("■ 无播放，清除状态"),
                    }
                    if publisher.set(target.clone()).await.is_err() {
                        eprintln!("Steam 后台任务已退出，结束。");
                        break;
                    }
                    current = target;
                }
            }
        }
    }

    Ok(())
}
