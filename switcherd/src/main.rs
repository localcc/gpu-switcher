use std::{
    future::pending,
    io::{self, ErrorKind},
    iter::once,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::WrapErr;
use minilsof::fileasync::LsofAsync;
use proto::GpuMode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    process::Command,
    time::sleep,
};
use tracing::{error, warn};
use zbus::{conn, interface};

pub mod pci;
pub use pci::*;
pub mod gpu;
pub use gpu::*;

#[derive(Serialize, Deserialize)]
enum HotplugType {
    Normal,
    Asus,
}

struct DedicatedGpuInfo {
    bus_id: PCIBusId,
    gpu_device: Option<GpuDevice>,
}

struct GpuSwitcher {
    dedicated_gpu_info: DedicatedGpuInfo,
    hotplug_type: HotplugType,
}

#[derive(Debug, zbus::DBusError)]
enum ColorEyreZbus {
    Eyre(String),
}

impl From<color_eyre::Report> for ColorEyreZbus {
    fn from(value: color_eyre::Report) -> Self {
        let lines = value
            .chain()
            .enumerate()
            .map(|(index, e)| format!("{}: {}", index, e))
            .collect::<Vec<_>>();
        ColorEyreZbus::Eyre(lines.join("\n"))
    }
}

#[interface(name = "cc.localcc.GpuSwitcher")]
impl GpuSwitcher {
    async fn get_mode(&self) -> GpuMode {
        match self.dedicated_gpu_info.gpu_device.as_ref() {
            None => GpuMode::Integrated,
            Some(e) => e
                .pci
                .get_driver()
                .await
                .map(|e| match e.as_str() {
                    "nvidia" => GpuMode::Nvidia,
                    "vfio-pci" => GpuMode::Vfio,
                    _ => GpuMode::Integrated,
                })
                .unwrap_or(GpuMode::Integrated),
        }
    }

    async fn set_mode(&mut self, mode: GpuMode, force: bool) -> Result<(), ColorEyreZbus> {
        self.set_gpu_mode(mode, force).await?;
        Ok(())
    }
}

enum ModprobeConfType {
    Vfio,
    Integrated,
    Nvidia,
}

enum IcdType {
    Inactive,
    Active,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum HotplugState {
    Unplug,
    Plug,
}

enum ServiceState {
    Stop,
    Start,
}

enum ServiceType {
    Nvidia,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DriverType {
    Vfio,
    Nvidia,
    None,
}

impl GpuSwitcher {
    const NVIDIA_DRIVERS: [&str; 5] = [
        "nvidia_uvm",
        "nvidia_drm",
        "nvidia_modeset",
        "nvidia",
        "nvidia-wmi-ec-backlight",
    ];

    const VFIO_DRIVERS: [&str; 4] = ["vfio_pci", "vfio_iommu_type1", "vfio_pci_core", "vfio"];

    async fn set_gpu_mode(&mut self, mode: GpuMode, force: bool) -> color_eyre::Result<()> {
        let current_mode = self.get_mode().await;
        if current_mode == mode {
            return Ok(());
        }

        match mode {
            GpuMode::Integrated => {
                self.rescan_pci(GpuMode::Integrated).await?;

                self.write_modprobe(ModprobeConfType::Integrated)
                    .await
                    .wrap_err("modprobe conf write")?;
                self.send_gpu_detach().await.wrap_err("send gpu detach")?;
                sleep(Duration::from_secs(5)).await;

                self.switch_services(ServiceState::Stop, ServiceType::Nvidia)
                    .await
                    .log_err();

                self.kill_gpu_use().await.wrap_err("kill gpu use")?;

                self.switch_icd(IcdType::Inactive).await.log_err();

                self.switch_drivers(DriverType::None, force)
                    .await
                    .wrap_err("switch drivers")?;

                let _ = self.hotplug_gpu(HotplugState::Plug).await;
                self.hotplug_gpu(HotplugState::Unplug)
                    .await
                    .wrap_err("hotplug unplug")?;
            }
            GpuMode::Nvidia => {
                self.rescan_pci(GpuMode::Nvidia).await?;

                self.write_modprobe(ModprobeConfType::Nvidia)
                    .await
                    .wrap_err("modprobe conf write")?;

                let need_hotplug = self
                    .get_hotplug_state()
                    .await
                    .map(|e| e != HotplugState::Plug)
                    .unwrap_or(true);
                if need_hotplug {
                    self.hotplug_gpu(HotplugState::Plug)
                        .await
                        .wrap_err("hotplug plug")?;
                }
                self.rescan_pci(mode).await.wrap_err("pci rescan")?;

                sleep(Duration::from_millis(500)).await;

                self.switch_drivers(DriverType::Nvidia, force)
                    .await
                    .wrap_err("switch drivers")?;

                self.switch_services(ServiceState::Start, ServiceType::Nvidia)
                    .await
                    .log_err();

                self.switch_icd(IcdType::Inactive).await.log_err();
            }
            GpuMode::Vfio => {
                self.rescan_pci(GpuMode::Vfio).await?;

                self.write_modprobe(ModprobeConfType::Vfio)
                    .await
                    .wrap_err("modprobe conf write")?;

                let need_hotplug = self
                    .get_hotplug_state()
                    .await
                    .map(|e| e != HotplugState::Plug)
                    .unwrap_or(true);
                if need_hotplug {
                    self.hotplug_gpu(HotplugState::Plug)
                        .await
                        .wrap_err("hotplug plug")?;
                }
                self.rescan_pci(mode).await.wrap_err("pci rescan")?;

                if current_mode == GpuMode::Nvidia {
                    self.send_gpu_detach().await.wrap_err("send gpu detach")?;
                    sleep(Duration::from_secs(5)).await;

                    self.switch_services(ServiceState::Stop, ServiceType::Nvidia)
                        .await
                        .log_err();

                    self.kill_gpu_use().await.wrap_err("kill gpu use")?;

                    self.switch_icd(IcdType::Inactive).await.log_err();
                }

                sleep(Duration::from_millis(500)).await;

                self.switch_drivers(DriverType::Vfio, force)
                    .await
                    .wrap_err("switch drivers")?;
            }
        }
        Ok(())
    }

    async fn write_modprobe(&mut self, ty: ModprobeConfType) -> Result<(), tokio::io::Error> {
        const NVIDIA_OSS_DRIVERS: [&str; 1] = ["nouveau"];

        const NVIDIA_MODESETTING: &str = r#"
        options nvidia-drm modeset=1
        "#;

        const NVIDIA_EC_BACKLIGHT: &str = r#"
        options nvidia-wmi-ec-backlight force=1
        "#;

        const MODPROBE_PATH: &str = "/etc/modprobe.d/gpu-switcherd.conf";

        let content = match ty {
            ModprobeConfType::Vfio | ModprobeConfType::Integrated => NVIDIA_OSS_DRIVERS
                .iter()
                .chain(Self::NVIDIA_DRIVERS.iter())
                .map(|e| format!("blacklist {}", e))
                .collect::<Vec<_>>()
                .join("\n"),
            ModprobeConfType::Nvidia => {
                let mut content = NVIDIA_OSS_DRIVERS
                    .iter()
                    .map(|e| format!("blacklist {}", e))
                    .collect::<Vec<_>>();
                content.push(NVIDIA_MODESETTING.to_owned());
                content.push(NVIDIA_EC_BACKLIGHT.to_owned());
                content.join("\n")
            }
        };

        let mut file = OpenOptions::new();
        let mut file = file
            .create(true)
            .write(true)
            .append(false)
            .truncate(true)
            .open(MODPROBE_PATH)
            .await?;

        file.write_all(content.as_bytes()).await?;

        Ok(())
    }

    async fn switch_icd(&mut self, new_state: IcdType) -> Result<(), SwitchIcdError> {
        macro_rules! nvidia_icd {
            () => {
                "/usr/share/vulkan/icd.d/nvidia_icd.json"
            };
        }
        const NVIDIA_ICD: &str = nvidia_icd!();
        const INACTIVE_ICD: &str = concat!(nvidia_icd!(), "_inactive");

        let nvidia_icd = Path::new(NVIDIA_ICD);
        let inactive_icd = Path::new(INACTIVE_ICD);

        let current_state = if nvidia_icd.exists() {
            Some(IcdType::Active)
        } else if inactive_icd.exists() {
            Some(IcdType::Inactive)
        } else {
            None
        };

        let Some(current_state) = current_state else {
            return Err(SwitchIcdError::IcdMissing);
        };

        match (current_state, new_state) {
            (IcdType::Inactive, IcdType::Active) => {
                tokio::fs::rename(inactive_icd, nvidia_icd)
                    .await
                    .map_err(SwitchIcdError::Rename)?;
            }
            (IcdType::Active, IcdType::Inactive) => {
                tokio::fs::rename(nvidia_icd, inactive_icd)
                    .await
                    .map_err(SwitchIcdError::Rename)?;
            }
            _ => {}
        };

        Ok(())
    }

    async fn hotplug_gpu(&mut self, ty: HotplugState) -> Result<(), io::Error> {
        match self.hotplug_type {
            HotplugType::Normal => {
                let Some(device) = self.dedicated_gpu_info.gpu_device.as_ref() else {
                    warn!("gpu not found while trying hotplug, ignoring");
                    return Ok(());
                };

                let Some(slot) = device.gpu_slot.as_ref() else {
                    warn!("gpu slot not found while doing normal hotplug, ignoring");
                    return Ok(());
                };

                let slot = Path::new(slot).join("power");

                let new_state = match ty {
                    HotplugState::Plug => true,
                    HotplugState::Unplug => false,
                };

                sleep(Duration::from_secs(1)).await;

                let state = match new_state {
                    false => b"0",
                    true => b"1",
                };

                tokio::fs::write(slot, state).await?;

                Ok(())
            }
            HotplugType::Asus => self.hotplug_asus(ty).await,
        }
    }

    async fn get_hotplug_state(&self) -> Result<HotplugState, io::Error> {
        match self.hotplug_type {
            HotplugType::Normal => {
                let Some(device) = self.dedicated_gpu_info.gpu_device.as_ref() else {
                    warn!("gpu not found while fetching hotplug state");
                    return Err(io::Error::new(ErrorKind::NotFound, ""));
                };

                let Some(slot) = device.gpu_slot.as_ref() else {
                    warn!("slot not found while fetching slot state");
                    return Err(io::Error::new(ErrorKind::NotFound, ""));
                };

                let slot = Path::new(slot).join("power");
                let current_state = tokio::fs::read_to_string(&slot).await?.starts_with("1");

                Ok(match current_state {
                    true => HotplugState::Plug,
                    false => HotplugState::Unplug,
                })
            }
            HotplugType::Asus => Ok(self.get_hotplug_state_asus().await?),
        }
    }

    async fn rescan_pci(&mut self, new_mode: GpuMode) -> Result<(), RescanPCIError> {
        tokio::fs::write("/sys/bus/pci/rescan", b"1")
            .await
            .map_err(RescanPCIError::Rescan)?;

        let requires_dgpu = match new_mode {
            GpuMode::Integrated => false,
            GpuMode::Nvidia | GpuMode::Vfio => true,
        };

        let pci_device = PCIDevice::find_device(self.dedicated_gpu_info.bus_id).await;
        let gpu = if requires_dgpu {
            let pci_device = pci_device.map_err(RescanPCIError::GpuNotFound)?;
            let gpu_device = GpuDevice::new(pci_device)
                .await
                .map_err(RescanPCIError::GpuCreation)?;

            Some(gpu_device)
        } else {
            match pci_device.ok() {
                Some(e) => GpuDevice::new(e).await.ok(),
                None => None,
            }
        };

        self.dedicated_gpu_info.gpu_device = gpu;

        Ok(())
    }

    async fn switch_services(
        &mut self,
        state: ServiceState,
        ty: ServiceType,
    ) -> Result<(), SwitchServicesError> {
        let services = match ty {
            ServiceType::Nvidia => &["nvidia-persistenced.service", "nvidia-powerd.service"],
        };

        let verb = match state {
            ServiceState::Stop => "stop",
            ServiceState::Start => "start",
        };

        let command = Command::new("systemctl")
            .arg(verb)
            .args(services)
            .status()
            .await?;

        if !command.success() {
            return Err(SwitchServicesError::ExitCode(command.code().unwrap()));
        }

        Ok(())
    }

    async fn send_gpu_detach(&mut self) -> Result<(), SendGpuDetachError> {
        let Some(gpu_device) = self.dedicated_gpu_info.gpu_device.as_mut() else {
            return Err(SendGpuDetachError::NoGpu);
        };

        gpu_device.send_detach().await?;

        Ok(())
    }

    async fn kill_gpu_use(&mut self) -> Result<(), KillGpuUseError> {
        let lsof = LsofAsync::new();

        let Some(gpu_device) = self.dedicated_gpu_info.gpu_device.as_mut() else {
            return Err(KillGpuUseError::NoGpu);
        };

        let devices = gpu_device
            .cards
            .iter()
            .map(|e| PathBuf::from(format!("/dev/dri/card{}", e)))
            .chain(
                gpu_device
                    .render_devices
                    .iter()
                    .map(|e| PathBuf::from(format!("/dev/dri/renderD{}", e))),
            )
            .chain(once(PathBuf::from("/dev/nvidia0")));

        for device in devices {
            let Some(e) = lsof
                .target_file_ls(device.clone())
                .await
                .context(format!("lsof for {}", device.display()))
                .log_warn()
            else {
                continue;
            };

            for process in e {
                let pid = process.pid.parse::<u32>().unwrap();

                // SAFETY: always safe to call
                let res = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                if res != 0 {
                    return Err(KillGpuUseError::KillProcess(
                        process
                            .name
                            .unwrap_or("unknown".to_string())
                            .into_boxed_str(),
                        res,
                    ));
                }
            }
        }

        Ok(())
    }

    async fn switch_drivers(
        &mut self,
        ty: DriverType,
        force: bool,
    ) -> Result<(), SwitchDriversError> {
        let Some(dedicated_gpu) = self.dedicated_gpu_info.gpu_device.as_mut() else {
            return Err(SwitchDriversError::NoGpu);
        };

        let current_type = dedicated_gpu
            .pci
            .get_driver()
            .await
            .map(|e| match e.as_str() {
                "nvidia" => DriverType::Nvidia,
                "vfio-pci" => DriverType::Vfio,
                _ => DriverType::None,
            })
            .unwrap_or(DriverType::None);

        if ty == current_type {
            return Ok(());
        }

        let driver_unload_list = match current_type {
            DriverType::Nvidia => Self::NVIDIA_DRIVERS.as_slice(),
            DriverType::Vfio => Self::VFIO_DRIVERS.as_slice(),
            DriverType::None => &[],
        };
        for driver in driver_unload_list {
            Self::unload_driver(driver, force).await?;
        }

        dedicated_gpu
            .unbind()
            .await
            .context("gpu unbind")
            .log_warn();

        match ty {
            DriverType::Vfio => {
                let mut modprobe = Command::new("modprobe");
                let modprobe = modprobe.arg("vfio_pci");
                let status = modprobe
                    .status()
                    .await
                    .map_err(SwitchDriversError::DriverLoad)?;

                if !status.success() {
                    return Err(SwitchDriversError::DriverLoad(io::Error::other(format!(
                        "exit code {}",
                        status.code().unwrap()
                    ))));
                }

                dedicated_gpu
                    .bind_vfio()
                    .await
                    .map_err(SwitchDriversError::DriverBind)?;
            }
            DriverType::Nvidia => {
                let mut modprobe = Command::new("modprobe");
                let modprobe = modprobe.arg("nvidia_drm").arg("nvidia_uvm");
                let status = modprobe
                    .status()
                    .await
                    .map_err(SwitchDriversError::DriverLoad)?;

                if !status.success() {
                    return Err(SwitchDriversError::DriverLoad(io::Error::other(format!(
                        "exit code {}",
                        status.code().unwrap()
                    ))));
                }
            }
            DriverType::None => {}
        }

        Ok(())
    }

    const ASUS_HOTPLUG_PATH: &str = "/sys/devices/platform/asus-nb-wmi/dgpu_disable";
    async fn hotplug_asus(&mut self, ty: HotplugState) -> Result<(), io::Error> {
        let state = tokio::fs::read_to_string(Self::ASUS_HOTPLUG_PATH).await?;
        let disabled = state.starts_with('1');

        let new_disabled = match ty {
            HotplugState::Unplug => true,
            HotplugState::Plug => false,
        };

        if new_disabled != disabled {
            let state = match new_disabled {
                true => "1",
                false => "0",
            };
            tokio::fs::write(Self::ASUS_HOTPLUG_PATH, state.as_bytes()).await?;
        }

        Ok(())
    }

    async fn get_hotplug_state_asus(&self) -> Result<HotplugState, io::Error> {
        let state = tokio::fs::read_to_string(Self::ASUS_HOTPLUG_PATH).await?;
        let disabled = state.starts_with('1');

        Ok(match disabled {
            true => HotplugState::Unplug,
            false => HotplugState::Plug,
        })
    }

    async fn unload_driver(name: &str, force: bool) -> Result<(), UnloadDriverError> {
        let mut rmmod = Command::new("rmmod");
        let rmmod = if force { rmmod.arg("-f") } else { &mut rmmod };
        let rmmod = rmmod.arg(name);

        let output = rmmod.output().await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("in use") {
            return Err(UnloadDriverError::DriverInUse(
                name.to_string().into_boxed_str(),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
enum UnloadDriverError {
    #[error("driver {0} is in use")]
    DriverInUse(Box<str>),
    #[error("io: {0:?}")]
    Io(#[from] tokio::io::Error),
}

#[derive(Debug, Error)]
enum SwitchIcdError {
    #[error("nvidia icd is missing")]
    IcdMissing,
    #[error("rename: {0:?}")]
    Rename(tokio::io::Error),
}

#[derive(Debug, Error)]
enum SwitchServicesError {
    #[error("unexpected exit code: {0}")]
    ExitCode(i32),
    #[error("io: {0:?}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
enum KillGpuUseError {
    #[error("no gpu")]
    NoGpu,
    #[error("lsof: {0:?}")]
    Lsof(#[from] minilsof::Error),
    #[error("kill process ({0:?}): failed with result {1:?}")]
    KillProcess(Box<str>, i32),
}

#[derive(Debug, Error)]
enum RescanPCIError {
    #[error("rescan: {0:?}")]
    Rescan(io::Error),
    #[error("gpu not found: {0:?}")]
    GpuNotFound(PCIFindDeviceError),
    #[error("gpu creation: {0:?}")]
    GpuCreation(GpuDeviceCreationError),
}

#[derive(Debug, Error)]
enum SendGpuDetachError {
    #[error("no gpu")]
    NoGpu,
    #[error("io: {0:?}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
enum SwitchDriversError {
    #[error("no gpu")]
    NoGpu,
    #[error("driver unload: {0:?}")]
    DriverUnload(#[from] UnloadDriverError),
    #[error("driver load: {0:?}")]
    DriverLoad(io::Error),
    #[error("driver bind: {0:?}")]
    DriverBind(io::Error),
}

#[derive(Serialize, Deserialize)]
struct Config {
    device_path: String,
    hotplug_type: HotplugType,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let cfg = fs::read_to_string("/etc/gpu-switcherd.conf")
        .await
        .wrap_err("config file read")?;
    let cfg = serde_json::from_str::<Config>(&cfg).wrap_err("config file parse")?;

    let pci_bus_id = PCIBusId::try_from(cfg.device_path.as_ref()).wrap_err("gpu device id")?;

    let dev = PCIDevice::find_device(pci_bus_id).await.log_warn();
    let gpu = match dev {
        Some(e) => Some(GpuDevice::new(e).await.wrap_err("gpu device creation")?),
        None => None,
    };

    let switcher = GpuSwitcher {
        dedicated_gpu_info: DedicatedGpuInfo {
            bus_id: pci_bus_id,
            gpu_device: gpu,
        },
        hotplug_type: HotplugType::Normal,
    };

    // input from user:
    //
    // 0000:64:00.0
    //
    // need from pci:
    //
    // pcibusid -> vid/pid
    //
    // need from gpu
    //
    // /sys/bus/pci/devices/${pcibusid}/drm/card*/uevent
    // /sys/bus/pci/devices/${pcibusid}/driver/unbind
    // /sys/bus/pci/devices/${pcibusid}/remove
    //
    // /sys/bus/pci/slots/${gpuslot}/power
    //
    // card number from: /sys/bus/pci/devices/${pcibusid}/drm/card*
    // render number from: /sys/bus/pci/devices/${pcibusid}/drm/renderD*
    //
    //

    // integrated->vfio
    //
    // write modprobe conf:
    //
    // blacklist [nouveau, nvidia_drm, nvidia_uvm, nvidia_modeset, nvidia, nvidia-wmi-ec-backlight]
    // options vfio-pci ids=10de:2f58
    //
    //
    // move /usr/share/vulkan/icd.d/nvidia_icd.json to /usr/share/vulkan/icd.d/nvidia_icd.json_inactive
    //
    // nonasus: hotplug plug (write 0 or 1 to /sys/bus/pci/slots/0-2/power, slot is determined by looking at /sys/bus/pci/slots/*/address)
    // asus hotplug plug: echo "0" > /sys/devices/platform/asus-nb-wmi/dgpu_disable
    //
    // rescan pci bus: echo 1 > /sys/bus/pci/rescan, reenumrate devices
    //
    // systemctl stop nvidia-persistenced
    // systemctl stop nvidia-powerd
    //
    // lsof /dev/nvidia0, kill each (allow ENOFILE fail)
    // lsof /dev/dri/card1 (or the one that belongs to nvidia), kill each (allow ENOFILE fail)
    //
    // load vfio-pci

    // vfio->integrated
    //
    // send detach event (echo "remove" > /sys/bus/pci/devices/0000:64:00.0/drm/card*/uevent)
    // lsof /dev/nvidia0, kill each (allow ENOFILE fail)
    // lsof /dev/dri/card1 (or the one that belongs to nvidia), kill each (allow ENOFILE fail)
    //
    // modprobe -r [vfio_pci, vfio_pci_core, vfio_iommu_type1, vfio_virqfd, vfio_mdev, vfio]
    // echo "0000:64:00.0" > /sys/bus/pci/devices/0000:64:00.0/driver/unbind
    // echo "1" > /sys/bus/pci/devices/0000:64:00.0/remove
    //
    // blacklist [nouveau, nvidia_drm, nvidia_uvm, nvidia_modeset, nvidia, nvidia-wmi-ec-backlight]
    //
    // nonasus: hotplug plug (write 0 or 1 to /sys/bus/pci/slots/0-2/power, slot is determined by looking at /sys/bus/pci/slots/*/address)
    // asus hotplug plug: echo "1" > /sys/devices/platform/asus-nb-wmi/dgpu_disable

    // integrated->nvidia
    //
    // write modprobe conf
    //
    // blacklist nouveau
    // alias nouveau off
    // options nvidia-drm modeset=1
    // options nvidia-wmi-ec-backlight force=1
    //
    //
    // move /usr/share/vulkan/icd.d/nvidia_icd.json_inactive to /usr/share/vulkan/icd.d/nvidia_icd.json
    //
    // nonasus: hotplug plug (write 0 or 1 to /sys/bus/pci/slots/0-2/power, slot is determined by looking at /sys/bus/pci/slots/*/address)
    // asus hotplug plug: echo "0" > /sys/devices/platform/asus-nb-wmi/dgpu_disable
    //
    // rescan pci bus: echo 1 > /sys/bus/pci/rescan, reenumrate devices
    //
    // modprobe nvidia_drm
    // modprobe nvidia_modeset
    // modprobe nvidia_uvm
    // modprobe nvidia
    // modprobe nvidia_wmi_ec_backlight
    //
    // systemctl start nvidia-persistenced
    // systemctl start nvidia-powerd
    //

    // nvidia->integrated
    //
    // systemctl stop nvidia-persistenced
    // systemctl stop nvidia-powerd
    //
    // send detach event (echo "remove" > /sys/bus/pci/devices/0000:64:00.0/drm/card*/uevent)
    // lsof /dev/nvidia0, kill each (allow ENOFILE fail)
    // lsof /dev/dri/card1 (or the one that belongs to nvidia), kill each (allow ENOFILE fail)
    //
    // modprobe nvidia_drm
    // modprobe nvidia_modeset
    // modprobe nvidia_uvm
    // modprobe nvidia
    // modprobe nvidia_wmi_ec_backlight
    //
    // echo "0000:64:00.0" > /sys/bus/pci/devices/0000:64:00.0/driver/unbind
    // echo "1" > /sys/bus/pci/devices/0000:64:00.0/remove
    //
    // blacklist [nouveau, nvidia_drm, nvidia_uvm, nvidia_modeset, nvidia, nvidia-wmi-ec-backlight]
    //
    // move /usr/share/vulkan/icd.d/nvidia_icd.json to /usr/share/vulkan/icd.d/nvidia_icd.json_inactive
    //
    // nonasus: hotplug plug (write 0 or 1 to /sys/bus/pci/slots/0-2/power, slot is determined by looking at /sys/bus/pci/slots/*/address)
    // asus hotplug plug: echo "1" > /sys/devices/platform/asus-nb-wmi/dgpu_disable

    let _conn = conn::Builder::system()
        .wrap_err("session builder")?
        .name(proto::SERVICE_NAME)
        .wrap_err("session name")?
        .serve_at("/cc/localcc/GpuSwitcher", switcher)
        .wrap_err("serve path")?
        .build()
        .await
        .wrap_err("connection creation")?;

    println!("serving!");

    pending::<()>().await;

    Ok(())
}

trait ResultPrint<T> {
    fn log_err(self) -> Option<T>;
    fn log_warn(self) -> Option<T>;
}

impl<T, E: std::fmt::Debug> ResultPrint<T> for Result<T, E> {
    fn log_err(self) -> Option<T> {
        match self {
            Ok(e) => Some(e),
            Err(e) => {
                error!("{:?}", e);
                None
            }
        }
    }

    fn log_warn(self) -> Option<T> {
        match self {
            Ok(e) => Some(e),
            Err(e) => {
                warn!("{:?}", e);
                None
            }
        }
    }
}
