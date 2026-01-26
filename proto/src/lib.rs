use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

pub const SERVICE_NAME: &str = "cc.localcc.GpuSwitcher";

#[derive(Debug, Copy, Clone, Type, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum GpuMode {
    Integrated,
    Nvidia,
    Vfio,
}
