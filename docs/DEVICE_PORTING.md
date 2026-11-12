# Device Porting Guide

**English** | [简体中文](DEVICE_PORTING.zh-CN.md)

This guide is for developers and agents adding another display. The current transport is a hardware-validated TM-360 configuration, not a universal MS9132 protocol.

## First principle

Do not send the current initialization reports based only on brand, appearance, USB VID/PID, or controller model. The known `345F:9132` identifier occurs on different resolutions and OEM products. Members of the MS9132 family also differ in frame-header high bits, ZLP, trigger, and keep-alive behavior.

Before official traffic captures are available, investigate a new device only through read-only descriptors. Hardware display writes, re-enumeration, driver installation, or USBPcap installation require the device owner's explicit authorization.

## Stage 1: Read-only identification

Record:

1. USB VID/PID, bcdDevice, serial number, Manufacturer/Product strings.
2. Configuration, interface, and endpoint descriptors.
3. HID usage page, report length, and interface number.
4. Bulk OUT endpoint, MPS, and the bound Windows driver.
5. Model/firmware strings shown by the official application.
6. Physical resolution, orientation, and official refresh modes.

VID/PID alone is insufficient. Match at least the interface topology and a product/firmware fingerprint. Reject display writes when the configuration cannot be distinguished.

## Stage 2: Capture the official protocol

Use USBPcap/Wireshark to capture:

1. From idle hardware through official application startup and its first displayed frame.
2. A continuous 10–30 second dynamic display session and normal application exit.

Keep the original pcapng, not just extracted hexadecimal. Identify:

- HID SetFeature/GetFeature order, 8-byte payloads, responses, and timing.
- Behavior before and after claiming the video interface.
- Preload, enable, or trigger operations around the first frame.
- Frame header, pixel payload, trailer, and total size.
- Bulk chunking, short packets, ZLP count, and trigger behavior.
- Periodic control reports, full-frame resends, and firmware behavior after stopping.

On Windows, HIDAPI/HidD buffers for an unnumbered report still start with report ID `00`. The USB control data stage may contain only the following eight bytes. Do not confuse the two representations.

## Stage 3: Determine the pixel format

Use solid colors and small positioning patterns to verify:

- RGB/BGR channel order.
- RGB565, RGB888, YUYV/UYVY, or other packing.
- Full versus limited range.
- Orientation, rotation, and mirroring.
- Row alignment, stride, and whether two pixels share chroma.

The current TM-360 path is limited-range UYVY422, in `U,Y0,V,Y1` order. Early DS339 assumptions about BGR888 were overturned by hardware tests; use both the observed display and traffic captures as evidence.

## Stage 4: Adapt the code

Before a second device is supported, there is no need to abstract the whole project. Once a second complete evidence set exists, extract the smallest device configuration from these locations:

| File | Device-specific details |
|---|---|
| `protocol.rs` | Resolution, VIC, pixel encoding, frame header/trailer |
| `transport.rs` | VID/PID, interfaces/endpoints, startup reports/timing, preload, chunks, ZLP/trigger |
| `display.rs` | Minimum resend interval and keep-alive behavior |
| `media.rs` / `render.rs` | Canvas size and default template geometry |
| Matching tests | Accepted profiles and explicit rejection reasons |

A minimal future shape is:

```text
DeviceProfile
  matcher              descriptors and interface topology
  width / height / vic display mode
  startup              capture-validated reports and timing
  pixel_format         pixel conversion
  frame_format         header, trailer, size fields
  bulk_policy          endpoint, chunk, ZLP/trigger
  keepalive            cached frame/control reports and intervals
```

Use configuration data for data-only differences. Add an enum or trait only for a real behavior difference in conversion or transport. Do not create a plugin system, configurable protocol byte sequences, or speculative initialization probes for hypothetical devices.

Selection must fail closed: zero matches or multiple matches means no writes. A configuration file must not bypass descriptor matching to force commands onto an unknown device.

## Stage 5: Tests

Offline coverage must include:

- Exact agreement with captured mode-report bytes.
- Encoding of known RGB colors.
- Packet size, header/trailer, and resolution fields.
- Chunk boundaries and ZLP/trigger counts.
- Rejection of known other configurations by the matcher.
- Protocol, rendering, and benchmark tests that do not enter the transport.

During an authorized hardware window, test in this order:

1. A static card: resolution, orientation, corners, and RGB colors.
2. A clock changing once per second: continuous display.
3. Low-rate static resends: keep-alive behavior.
4. Animation: chunking, throughput, and stability.
5. Normal exit: time to logo, black screen, or retention of the last frame.
6. Restart the official application: confirm device recovery.

Use only captured and explained runtime commands. Do not introduce firmware, EEPROM, descriptor writes, or USB reset paths.

## Acceptance criteria

A new configuration may be called supported only when all of these hold:

- Reproducible descriptors and official communication evidence are available.
- Offline protocol tests pass.
- At least one physical unit passes color, orientation, animation, keep-alive, and exit/recovery tests.
- The README identifies the product, hardware revision, and matching fingerprint precisely.
- The existing TM-360 matcher remains equally strict.

An implementation without hardware acceptance is experimental and excluded from automatic matching by default.
