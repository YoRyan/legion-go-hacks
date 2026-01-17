This repository documents my ongoing attempt to turn the 1st-generation Lenovo Legion Go into a modern Linux ultra-mobile PC (UMPC) with a gorgeous high-resolution display and three operating modes (gaming, laptop, tablet).

The target platform is Fedora Silverblue with the GNOME desktop. I use a 3D-printed keyboard cover sold by [Tango Tactical](https://www.etsy.com/listing/1897686079/lenovo-legion-go-1-keyboard-attachment) that connects via Bluetooth.

## Disk encryption

A must for any portable device, yet this is very tricky to accomplish on the LGo because there is no native keyboard. To type a passphrase to perform a LUKS unlock, you have to plug in an external USB keyboard. (To add insult to injury, the unlock screen is in portrait orientation.)

Automatic unlock via TPM is one option, but this requires Secure Boot, which is also very tricky to achieve if you are using anything but the stock, Microsoft-signed Fedora kernel. Secure Boot also [cannot guarantee](https://github.com/fedora-silverblue/silverblue-docs/pull/176) the integrity of the kernel command line, among other things, because that would break the Silverblue boot process.

Currently, I unlock with a keyfile stored on a USB drive attached to my keyring. This can be done keyboard-free on Silverblue with just a couple of additions to kargs.

## Auto-rotate the screen

The LGo uses a display derived from a tablet, and this display's native orientation is left-side-up (relative to the kickstand). Contemporary kernels do a good job keeping the display in landscape mode, but it can still revert to portrait during the boot process and sometimes after a suspend/resume cycle.

Sometimes we actually do want to be in portrait mode, too, like when using the device as a handheld tablet.

All the accelerometers work out of the box, so GNOME has all the information it needs to rotate the screen for us. Unfortunately, GNOME will only do so if there is an onboard input device that emits the `SW_TABLET_MODE` event. This is not true for most x86 convertibles, and certainly not for the LGo.

The solution: Build a fake one. The `tablet-mode` executable creates an evdev input device whose sole purpose is to emit `SW_TABLET_MODE(1)` and force GNOME permanently into tablet mode. This turns on GNOME's auto-rotation feature.

udev [includes](https://github.com/systemd/systemd/blob/main/hwdb.d/60-sensor.hwdb) an for the LGo that appears to swap around the accelerometer's values, to account for Linux running the display in landscape rather than portrait. If we don't undo this, GNOME will constantly be 90-degrees off when it changes the display orientation. This is the purpose of `60-sensor-local.hwdb`: to change `ACCEL_MOUNT_MATRIX` for this sensor back to the identity matrix.

## Show the on-screen keyboard

If we enable tablet mode,  as above, we also gain access to GNOME's on-screen keyboard, making the LGo usable without any kind of physical keyboard.

Unfortunately, if you do happen to be using a physical keyboard, the OSK appears whenever you tap on an input field or when you swipe up from the bottom edge of the screen. This is slightly annoying, but I personally don't consider it a dealbreaker. In an earlier version of `tablet-mode`, I counted the number of attached keyboards via libinput and attempted to switch tablet mode on/off based on whether an external keyboard was attached. This solution did not work because when leaving tablet mode, Mutter [automatically switches](https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/backends/meta-monitor-manager.c) the display back to its "normal" orientation. And on the LGo, that orientation... is portrait, not landscape.

One potential solution is to block the OSK with an extension when an external keyboard is attached. This [appears to be possible](https://github.com/alexcanepa/cariboublocker/tree/master/49) on current versions of GNOME.
