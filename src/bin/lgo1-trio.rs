use dbus::blocking::Connection;
use dbus::message::MatchRule;
use dbus_crossroads::{Crossroads, IfaceBuilder};
use evdev::{AttributeSet, BusType, Device, KeyCode, SwitchCode, uinput::VirtualDevice};
use libc::{POLLIN, nfds_t, pollfd, ppoll};
use std::collections::HashSet;
use std::error::Error;
use std::io;
use std::ptr;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use udev::{EventType, MonitorBuilder};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

enum KeyboardStatus {
    /// The keyboard case is connected.
    CaseExternal = 0x2,
    /// Any external keyboard, excluding the keyboard case, is connected.
    AnyExternal = 0x1,
    /// No external keyboard is connected.
    None = 0x0,
}

struct DBusObject {
    keyboard_status: u32,
}

const DBUS_OBJECT_PATH: &str = "/com/youngryan/LGo1Trio";

fn main() {
    let (udev_s, udev_r) = mpsc::sync_channel::<()>(0);
    spawn_loop("read_udev_events", move || read_udev_events(&udev_s));

    let crossroads = Arc::new(Mutex::new(make_dbus_crossroads()));
    let crossroads2 = crossroads.clone();
    spawn_loop("read_keyboard_status", move || {
        read_keyboard_status(&udev_r, &crossroads)
    });
    spawn_loop("run_dbus", move || run_dbus(&crossroads2));

    let _ = spawn_loop("run_virtual_device", run_virtual_device).join();
    unreachable!();
}

/// Spawn a new thread in an infinite loop with error reporting.
fn spawn_loop<F, T>(name: &'static str, f: F) -> thread::JoinHandle<T>
where
    F: Fn() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    thread::spawn(move || {
        loop {
            match f() {
                Ok(_) => {}
                Err(err) => eprintln!("Error in {}: {}", name, err),
            }
            thread::sleep(Duration::from_secs(10));
        }
    })
}

fn make_dbus_crossroads() -> Crossroads {
    let mut cr = Crossroads::new();
    let iface_token = cr.register(
        "com.youngryan.LGo1Trio",
        |b: &mut IfaceBuilder<DBusObject>| {
            b.property("KeyboardStatus")
                .get(|_, obj| Ok(obj.keyboard_status));
        },
    );
    cr.insert(
        DBUS_OBJECT_PATH,
        &[iface_token],
        DBusObject {
            keyboard_status: KeyboardStatus::None as u32,
        },
    );
    cr
}

fn read_udev_events(notify: &mpsc::SyncSender<()>) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    let socket = MonitorBuilder::new()?.match_subsystem("input")?.listen()?;

    let mut fds = vec![pollfd {
        fd: socket.as_raw_fd(),
        events: POLLIN,
        revents: 0,
    }];

    loop {
        let result = unsafe {
            ppoll(
                (&mut fds[..]).as_mut_ptr(),
                fds.len() as nfds_t,
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if result < 0 {
            return Err(From::from(io::Error::last_os_error()));
        }
        let event = match socket.iter().next() {
            Some(evt) => evt,
            None => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        match event.event_type() {
            EventType::Add | EventType::Remove => {
                let _ = notify.try_send(());
            }
            _ => {}
        }
    }
}

fn read_keyboard_status(wait: &mpsc::Receiver<()>, cr: &Arc<Mutex<Crossroads>>) -> Result<()> {
    let cr = cr.clone();
    loop {
        // Wait for an update, but also force a recheck every now and then.
        match wait.recv_timeout(Duration::from_secs(120)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                // Wait for all events to come in, and then impose a short delay. This
                // accounts for the time the kernel needs to add and remove devices.
                loop {
                    match wait.recv_timeout(Duration::from_millis(1000)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        _ => continue,
                    }
                }
            }
        }

        let status = keyboard_status(evdev::enumerate().map(|t| t.1).collect());
        let mut cr_lock = cr.lock().unwrap();
        let obj: &mut DBusObject = cr_lock.data_mut(&DBUS_OBJECT_PATH.into()).unwrap();
        obj.keyboard_status = status as u32;
    }
}

fn keyboard_status(evdev_devices: Vec<Device>) -> KeyboardStatus {
    const TEST_KEYS: [KeyCode; 3] = [KeyCode::KEY_ENTER, KeyCode::KEY_BACKSPACE, KeyCode::KEY_ESC];
    const INTERNAL_BLACKLIST: [(BusType, u16, u16); 2] = [
        (BusType::BUS_I8042, 0x1, 0x1),     // AT Translated Set 2 keyboard
        (BusType::BUS_USB, 0x17ef, 0x6184), // Legion-Controller 1-B0 Keyboard
    ];
    let internal_blacklist: HashSet<(u16, u16, u16)> = INTERNAL_BLACKLIST
        .iter()
        .map(|&(bus_type, vendor, product)| (bus_type.0, vendor, product))
        .collect();

    for d in evdev_devices.iter() {
        let id = d.input_id();
        let id_t = (id.bus_type().0, id.vendor(), id.product());
        if id_t == (BusType::BUS_BLUETOOTH.0, 0x04e8, 0x7021) {
            return KeyboardStatus::CaseExternal;
        }

        let looks_like_keyboard = match d.supported_keys() {
            Some(s) => TEST_KEYS.iter().all(|&k| s.contains(k)),
            None => false,
        };
        let is_blacklisted = internal_blacklist.contains(&id_t);
        if looks_like_keyboard && !is_blacklisted {
            return KeyboardStatus::AnyExternal;
        }
    }
    KeyboardStatus::None
}

fn run_dbus(cr: &Arc<Mutex<Crossroads>>) -> Result<()> {
    use dbus::channel::MatchingReceiver;

    let c = Connection::new_system()?;
    c.request_name("com.youngryan.LGo1Trio", false, true, false)?;

    let cr = cr.clone();
    c.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, conn| {
            let mut cr_lock = cr.lock().unwrap();
            let _ = cr_lock.handle_message(msg, conn);
            true
        }),
    );
    loop {
        c.process(Duration::from_secs(60))?;
    }
}

fn run_virtual_device() -> Result<()> {
    const FORWARD_KEYS: [KeyCode; 2] = [KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_VOLUMEUP];
    let forward_codes: HashSet<u16> = FORWARD_KEYS.iter().map(|k| k.0).collect();

    let mut internal_keyboard = evdev::enumerate()
        .map(|t| t.1)
        .find(|device| {
            let id = device.input_id();
            id.bus_type() == BusType::BUS_I8042 && id.vendor() == 0x1 && id.product() == 0x1
        })
        .ok_or("could not find internal keyboard")?;

    let keys = AttributeSet::<KeyCode>::from_iter(FORWARD_KEYS.iter());
    let switches = AttributeSet::<SwitchCode>::from_iter([SwitchCode::SW_TABLET_MODE]);
    let mut device = VirtualDevice::builder()?
        .name("lgo1-trio virtual input device")
        .with_keys(&keys)?
        .with_switches(&switches)?
        .build()?;
    device.emit(&[evdev::InputEvent::new(
        evdev::EventType::SWITCH.0,
        evdev::SwitchCode::SW_TABLET_MODE.0,
        1,
    )])?;

    loop {
        for event in internal_keyboard.fetch_events()? {
            let code = event.code();
            if forward_codes.contains(&code) {
                device.emit(&[evdev::InputEvent::new(
                    evdev::EventType::KEY.0,
                    code,
                    event.value(),
                )])?;
            }
        }
    }
}
