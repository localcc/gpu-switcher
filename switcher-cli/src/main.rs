use clap::Parser;
use color_eyre::eyre::Context;
use proto::GpuMode;
use zbus::{proxy, Connection};

#[proxy(
    interface = "cc.localcc.GpuSwitcher",
    default_service = "cc.localcc.GpuSwitcher",
    default_path = "/cc/localcc/GpuSwitcher"
)]
trait GpuSwitcher {
    async fn get_mode(&self) -> zbus::Result<GpuMode>;
    async fn set_mode(&mut self, mode: GpuMode, force: bool) -> zbus::Result<()>;
}

#[derive(Parser, Debug)]
#[command(version)]
enum Args {
    GetMode,
    SetMode {
        mode: GpuMode,
        #[arg(short, long, default_value_t = false)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = Args::parse();

    let connection = Connection::system().await.context("zbus connection")?;

    let mut proxy = GpuSwitcherProxy::new(&connection)
        .await
        .context("proxy creation")?;

    match args {
        Args::GetMode => {
            let mode = proxy.get_mode().await.context("getting mode")?;
            println!("current mode: {:?}", mode);
        }
        Args::SetMode { mode, force } => {
            proxy.set_mode(mode, force).await.context("setting mode")?;
            println!("mode set successfully!");
        }
    }

    Ok(())
}
