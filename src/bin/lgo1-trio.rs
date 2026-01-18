use evdev::{AttributeSet, BusType, Device, KeyCode, SwitchCode, uinput::VirtualDevice};
use std::collections::HashSet;
use std::io;
use std::thread;
use std::time::Duration;

const FORWARD_KEYS: [KeyCode; 2] = [KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_VOLUMEUP];

fn main() {
    let mut virtual_device = create_virtual_device().unwrap();
    write_tablet_mode(&mut virtual_device, 1).unwrap();

    loop {
        forward_suppressed_keys(&mut virtual_device);
        thread::sleep(Duration::from_secs(10));
    }
}

fn create_virtual_device() -> io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for key in FORWARD_KEYS {
        keys.insert(key);
    }

    let mut switches = AttributeSet::<SwitchCode>::new();
    switches.insert(SwitchCode::SW_TABLET_MODE);

    VirtualDevice::builder()?
        .name("lgo1-trio virtual input device")
        .with_keys(&keys)?
        .with_switches(&switches)?
        .build()
}

fn write_tablet_mode(device: &mut VirtualDevice, value: i32) -> io::Result<()> {
    println!("SW_TABLET_MODE {}", value);

    let event = evdev::InputEvent::new(
        evdev::EventType::SWITCH.0,
        evdev::SwitchCode::SW_TABLET_MODE.0,
        value,
    );
    device.emit(&[event])
}

fn forward_suppressed_keys(virtual_device: &mut VirtualDevice) {
    let mut forward_codes = HashSet::new();
    for key in FORWARD_KEYS {
        forward_codes.insert(key.0);
    }

    let mut internal_keyboard = match get_internal_keyboard() {
        Some(kbd) => kbd,
        None => return,
    };
    println!(
        "Found internal keyboard {}",
        internal_keyboard.name().unwrap_or_default()
    );

    loop {
        let fetch = match internal_keyboard.fetch_events() {
            io::Result::Ok(f) => f,
            io::Result::Err(_) => return,
        };
        for event in fetch {
            let code = event.code();
            if forward_codes.contains(&code) {
                match write_keycode(virtual_device, code, event.value()) {
                    io::Result::Ok(()) => {}
                    io::Result::Err(_) => return,
                }
            }
        }
    }
}

fn get_internal_keyboard() -> Option<Device> {
    evdev::enumerate().map(|t| t.1).find(|device| {
        let id = device.input_id();
        id.bus_type() == BusType::BUS_I8042
            && id.vendor() == 0x1
            && id.product() == 0x1
            && id.version() == 0xab83
    })
}

fn write_keycode(device: &mut VirtualDevice, code: u16, value: i32) -> io::Result<()> {
    println!("KEY {} 1", code);

    let event = evdev::InputEvent::new(evdev::EventType::KEY.0, code, value);
    device.emit(&[event])
}
