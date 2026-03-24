This repository documents my ongoing attempt to turn the 1st-generation Lenovo Legion Go into a modern Linux ultra-mobile PC (UMPC) with a gorgeous high-resolution display and three operating modes (gaming, laptop, tablet).

[Other efforts](https://github.com/aarron-lee/legion-go-tricks) have focused on porting SteamOS to this device and reinstating all the "gamer" features, like RGB lighting and fan curves. I am focusing on the desktop and tablet use cases.

The target platform is Fedora Silverblue with the GNOME desktop. I use a 3D-printed keyboard cover sold by [Tango Tactical](https://www.etsy.com/listing/1897686079/lenovo-legion-go-1-keyboard-attachment) that connects via Bluetooth.

## Disk encryption

A must for any portable device, yet this is very tricky to accomplish on the LGo because there is no native keyboard. To type a passphrase and perform a LUKS unlock, you need to connect an external USB keyboard. (To add insult to injury, the unlock screen is in portrait orientation.)

Automatic unlock via TPM is one option, but this requires Secure Boot, which is also very tricky to achieve if you are using anything but the stock, Microsoft-signed Fedora kernel. Secure Boot also [cannot guarantee](https://github.com/fedora-silverblue/silverblue-docs/pull/176) the integrity of the kernel command line, among other things, as that would break the Silverblue boot process.

Currently, I unlock with a keyfile stored on a USB drive attached to my keyring. This can be done keyboard-free on Silverblue with just a couple of additions to kargs:

```
rd.luks.options=discard,keyfile-timeout=10s
rd.luks.uuid=luks-LUKS_UUID rd.luks.key=LUKS_UUID=/keyfile:UUID=USB_UUID
```

## mt7921e latency spike fix

Add `mt7921e.disable_aspm=1` to kargs. This does not make the crappy Mediatek card perform as well as an Intel one, but (I think) it helps.

## tablet-switch.service

Although all of the LGo's accelerometers work out of the box, GNOME does not consider the device a convertible, and therefore will not auto-rotate the screen or display the on-screen keyboard. GNOME will only do these things if one of the connected input devices emits the `SW_TABLET_MODE` event. (This is not true of many x86 convertibles, and certainly not of the LGo.)

This daemon creates a virtual input device that emits the `SW_TABLET_MODE` event based on the presence of an external input device: tablet mode *on* when none are connected, and tablet mode *off* when any external device is connected. 

With a Rust toolchain installed, build the daemon with `cargo build --release`.

## tablet-switch.conf

This is the configuration file for the tablet-switch daemon. You can place it anywhere convenient in `/etc`.

This TOML file dictates how tablet-switch classifies peripherals into internally- and externally-connected input devices. The file contains rules that match evdev devices by bus type, vendor, product, and version numbers. (Rules need not specify all attributes; those that are present will be tested.) The file contains two sets of rules: the "case" set and the "internal" set. Devices that are classified as "case" will always force tablet mode off, and in the future may also trigger a "laptop in case" mode. Tablet mode turns on if there is *at least one* device present that is not classified as "internal." Rules are specified as key-value tables. The key is solely for documentation purposes and has no significant meaning.

Setting `debug = true` causes tablet-switch to print the classification of each connected device to assist in troubleshooting.

## 61-sensor-local.hwdb

The LGo's tablet-derived display is so-called "native portrait," meaning that the default mode is 90-degrees sideways relative to the kickstand orientation. Linux automatically rotates the video output to landscape at boot-time, and udev [includes](https://github.com/systemd/systemd/blob/main/hwdb.d/60-sensor.hwdb) an accelerometer adjustment so that the accelerometer's computed angle matches the rotated landscape angle. Unfortunately, when we enable tablet mode and allow Mutter to auto-rotate the screen, it does not take into account the boot-time adjustment and consequently is 90-degrees off when rotating the display.

This file, to be placed in `/etc/udev/hwdb.d`, reverts the `ACCEL_MOUNT_MATRIX` value for this sensor back to the identity matrix, so that Mutter computes the correct angle. To apply the fix, run `systemd-hwdb update && udevadm trigger`.

On Silverblue, it appears you also have to `systemctl mask systemd-hwdb-update.service` to prevent systemd from discarding this change at bootup.

## local-overrides.quirks

Entering tablet mode causes libinput to suppress all keyboard and touchpad events, in line with the authors' assumption that any device with a tablet mode switch would be a reverse-folding convertible. With this file placed in `/etc/libinput`, we apply the `ModelTabletModeNoSuspend` [quirk](https://wayland.freedesktop.org/libinput/doc/latest/device-quirks.html) so that libinput does not suppress the side volume keys or the righthand controller's mouse controls, which are considered by libinput to be a "keyboard" and "touchpad."

## mutter-49.5-dont-reset-panel-rotation.patch

A recent [commit](https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/4119/diffs?commit_id=e37021007de67e9358c9429fdf4f1f022a9c3ae3) changed Mutter to rotate the display to its native orientation when leaving tablet mode. This is a bad idea for a device that has a native portrait display, like the LGo, because it causes Mutter to rotate the display sideways. This patch deletes the code that causes the rotation; it is current for the version of Mutter shipped with Fedora 43. You can [build](https://blog.aloni.org/posts/how-to-easily-patch-fedora-packages/) your own custom package using fedpkg, and on Silverblue, you can install the resulting packages with something like:

```
# rpm-ostree override replace ./mutter-49.5-1.fc43.yoryan.x86_64.rpm ./mutter-common-49.5-1.fc43.yoryan.noarch.rpm
```
