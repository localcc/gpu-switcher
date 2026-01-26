# gpu-switcher

A utility for switching dedicated GPUs on/off on laptops.

## Installation

### Nix

Add the flake as a nix input:

```nix
inputs = {
	gpu-switcher.url = "github:localcc/gpu-switcher";
};
```

Use it in your config:

```nix
imports = [
	inputs.gpu-switcher.nixosModules.default
];

services.gpu-switcher = {
	enable = true;
	settings = {
		device_path = "0000:64:00.0";
	};
};
```

### Others

idk


## Configuration

To get the gpu device path use `lspci -nnnnv` and find your GPU there:

```
64:00.0 VGA compatible controller [0300]: NVIDIA Corporation GB205M [GeForce RTX 5070 Ti Mobile] [10de:2f58] (rev a1) (prog-if 00 [VGA controller])
	Subsystem: ASUSTeK Computer Inc. Device [1043:39a8]
	Physical Slot: 0-2
	Flags: bus master, fast devsel, latency 0, IRQ 133, IOMMU group 18
	Memory at d8000000 (32-bit, non-prefetchable) [size=64M]
	Memory at 7800000000 (64-bit, prefetchable) [size=16G]
	Memory at 7c00000000 (64-bit, prefetchable) [size=32M]
	I/O ports at f000 [size=128]
```

## Usage

To get the current gpu mode:

```
switcher-cli get-mode
```

To set a new gpu mode:

```
switcher-cli set-mode integrated/vfio/nvidia
```
