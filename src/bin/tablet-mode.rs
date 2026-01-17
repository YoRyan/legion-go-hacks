use evdev::{AttributeSet, SwitchCode, uinput::VirtualDevice};
use std::thread;
use std::time::Duration;

fn main() {
    let mut device = create_virtual_device().unwrap();
    write_tablet_mode(&mut device, 1);

    thread::sleep(Duration::MAX);
}

fn create_virtual_device() -> std::io::Result<VirtualDevice> {
    let mut switches = AttributeSet::<SwitchCode>::new();
    switches.insert(SwitchCode::SW_TABLET_MODE);

    VirtualDevice::builder()?
        .name("Virtual Tablet Mode")
        .with_switches(&switches)?
        .build()
}

fn write_tablet_mode(device: &mut VirtualDevice, value: i32) {
    let event = evdev::InputEvent::new(
        evdev::EventType::SWITCH.0,
        evdev::SwitchCode::SW_TABLET_MODE.0,
        value,
    );
    device.emit(&[event]).unwrap();
    println!("SW_TABLET_MODE {}", value);
}
