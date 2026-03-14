use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use evdev::{BusType, InputEvent, SwitchCode};
use serde::Deserialize;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// The keyboard case is connected.
    LaptopWithCase = 0x2,
    /// Any external input device, excluding the keyboard case, is connected.
    Laptop = 0x1,
    /// No external input device is connected.
    Tablet = 0x0,
}

#[derive(Deserialize)]
struct Config {
    debug: Option<bool>,
    internal: HashMap<String, Rule>,
    case: HashMap<String, Rule>,
}

impl Config {
    fn debug_mode(&self) -> bool {
        self.debug.is_some_and(|v| v)
    }

    fn is_internal_device(&self, device: &evdev::Device) -> bool {
        match device.input_id().bus_type() {
            BusType::BUS_VIRTUAL | BusType::BUS_HOST => true,
            bt if bt.0 == 0 => true,
            _ => self.internal.values().any(|id| id.match_device(device)),
        }
    }

    fn is_case_device(&self, device: &evdev::Device) -> bool {
        self.case.values().any(|id| id.match_device(device))
    }
}

#[derive(Deserialize)]
struct Rule {
    bus_type: Option<u16>,
    vendor: Option<u16>,
    product: Option<u16>,
    version: Option<u16>,
    with_keys: Option<Vec<u16>>,
    without_keys: Option<Vec<u16>>,
}

impl Rule {
    fn match_device(&self, device: &evdev::Device) -> bool {
        let id = device.input_id();
        let device_keys: HashSet<u16> = match device.supported_keys() {
            Some(keys) => keys.iter().map(|kc| kc.0).collect(),
            None => HashSet::<u16>::new(),
        };
        self.bus_type.is_none_or(|v| v == id.bus_type().0)
            && self.vendor.is_none_or(|v| v == id.vendor())
            && self.product.is_none_or(|v| v == id.product())
            && self.version.is_none_or(|v| v == id.version())
            && self
                .with_keys
                .clone()
                .is_none_or(|test_keys| test_keys.iter().all(|tk| device_keys.contains(tk)))
            && self
                .without_keys
                .clone()
                .is_none_or(|test_keys| test_keys.iter().all(|tk| !device_keys.contains(tk)))
    }
}

fn main() {
    let config = read_config();

    // (We pass references to make the functions callable multiple times.)

    let (virtual_s, virtual_r) = mpsc::channel::<InputEvent>();
    spawn_loop("run_virtual_device", move || run_virtual_device(&virtual_r));

    let (udev_s, udev_r) = mpsc::sync_channel::<()>(0);
    spawn_loop("read_udev_add_remove", move || {
        read_udev_add_remove(&udev_s)
    });
    let _ = spawn_loop("set_tablet_switch", move || {
        set_tablet_switch(&config, &udev_r, &virtual_s)
    })
    .join();

    unreachable!();
}

fn read_config() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.len() {
        2 => &args[1],
        _ => panic!("Arguments: {} <path to config file>", args[0]),
    };
    let str = std::fs::read_to_string(path).unwrap();
    toml::from_str(&str).unwrap()
}

/// Spawn a new thread in an infinite loop with error reporting.
fn spawn_loop<F, T>(name: &'static str, mut f: F) -> thread::JoinHandle<T>
where
    F: FnMut() -> Result<T> + Send + 'static,
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

fn run_virtual_device(event_stream: &mpsc::Receiver<InputEvent>) -> Result<()> {
    let switches = evdev::AttributeSet::<SwitchCode>::from_iter([SwitchCode::SW_TABLET_MODE]);
    let mut device: evdev::uinput::VirtualDevice = evdev::uinput::VirtualDevice::builder()?
        .name("tablet-switch virtual input device")
        .input_id(evdev::InputId::new(
            BusType::BUS_VIRTUAL,
            0x1234,
            0x5678,
            0x1,
        ))
        .with_switches(&switches)?
        .build()?;

    loop {
        let event = event_stream.recv()?;
        device.emit(&[event])?;
    }
}

fn read_udev_add_remove(consumer: &mpsc::SyncSender<()>) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    let socket = udev::MonitorBuilder::new()?
        .match_subsystem("input")?
        .listen()?;

    let mut fds = vec![libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];

    loop {
        let result = unsafe {
            libc::ppoll(
                (&mut fds[..]).as_mut_ptr(),
                fds.len() as libc::nfds_t,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(From::from(std::io::Error::last_os_error()));
        }
        let event = match socket.iter().next() {
            Some(evt) => evt,
            None => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        match event.event_type() {
            udev::EventType::Add | udev::EventType::Remove => {
                let _ = consumer.try_send(());
            }
            _ => {}
        }
    }
}

fn set_tablet_switch(
    config: &Config,
    udev_add_remove: &mpsc::Receiver<()>,
    virtual_consumer: &mpsc::Sender<InputEvent>,
) -> Result<()> {
    loop {
        let mode = current_mode(&config);
        virtual_consumer.send(InputEvent::new(
            evdev::EventType::SWITCH.0,
            SwitchCode::SW_TABLET_MODE.0,
            (mode == Mode::Tablet) as i32,
        ))?;

        if config.debug_mode() {
            eprintln!("> Detected mode: {:?}", mode);
        }

        // Wait for an update, but also force a recheck every now and then.
        match udev_add_remove.recv_timeout(Duration::from_secs(120)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                // Wait for all events to come in, and then impose a short delay. This
                // accounts for the time the kernel needs to add and remove devices.
                loop {
                    match udev_add_remove.recv_timeout(Duration::from_millis(1000)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        _ => continue,
                    }
                }
            }
        }
    }
}

fn current_mode(config: &Config) -> Mode {
    let devices: Vec<evdev::Device> = evdev::enumerate().map(|(_, d)| d).collect();

    if config.debug_mode() {
        for d in devices.iter() {
            let input_id = d.input_id();
            if config.is_case_device(&d) {
                eprintln!("* Case device {:?}", input_id);
            } else if config.is_internal_device(&d) {
                eprintln!("- Internal device {:?}", input_id);
            } else {
                eprintln!("+ External device {:?}", input_id);
            }
        }
    }

    for d in devices.iter() {
        if config.is_case_device(&d) {
            return Mode::LaptopWithCase;
        } else if !config.is_internal_device(&d) {
            return Mode::Laptop;
        }
    }
    Mode::Tablet
}
