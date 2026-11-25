//! A retained MIDI consumer session, independent of the control socket.
//!
//! Connect before issuing input that may produce MIDI: incoming messages are
//! queued only while the session's native subscriptions are active.
use std::sync::Arc;

use l6max_host::{host_midi::consumer, usb::UsbHost};

pub use l6max_host::host_midi::consumer::ControlChange;

/// An active consumer with its own subscription and pending receive queue.
pub struct MidiSession {
    client: Box<dyn consumer::Client>,
}

impl MidiSession {
    /// Connect to a native CoreMIDI or ALSA endpoint by its full name.
    pub fn connect_native(endpoint: &str) -> Result<Self, String> {
        Ok(Self {
            client: consumer::connect(endpoint)?,
        })
    }

    /// Connect to an in-process USB host and send MIDI on the given cable.
    pub fn connect_direct(usb: Arc<UsbHost>, cable: u8) -> Result<Self, String> {
        Ok(Self {
            client: Box::new(consumer::DirectUsbClient::connect(usb, cable)?),
        })
    }

    pub fn send_control_change(&mut self, message: ControlChange) -> Result<(), String> {
        self.client.send(&message.to_bytes()?)
    }

    pub fn poll_control_change(&mut self) -> Result<Option<ControlChange>, String> {
        self.client
            .poll_control_change()?
            .map(ControlChange::from_bytes)
            .transpose()
    }

    /// Poll the legacy three-byte control observation used by existing MIDI
    /// state checks. On ALSA this also reflects other control-shaped events;
    /// use `poll_control_change` when strict CC filtering is required.
    pub fn poll_control_observation(&mut self) -> Result<Option<[u8; 3]>, String> {
        self.client.poll()
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.client.send_bytes(bytes)
    }

    pub fn poll_bytes(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.client.poll_bytes()
    }

    /// Release native subscriptions while retaining the endpoint observer.
    pub fn release(&mut self) {
        self.client.release();
    }

    /// Return `None` when the transport cannot observe endpoint presence.
    pub fn endpoint_present(&mut self) -> Option<bool> {
        self.client.endpoint_present()
    }
}

impl consumer::Client for MidiSession {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.client.send(bytes)
    }

    fn poll(&mut self) -> Result<Option<[u8; 3]>, String> {
        self.client.poll()
    }

    fn poll_control_change(&mut self) -> Result<Option<[u8; 3]>, String> {
        self.client.poll_control_change()
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.client.send_bytes(bytes)
    }

    fn poll_bytes(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.client.poll_bytes()
    }

    fn release(&mut self) {
        self.client.release();
    }

    fn endpoint_present(&mut self) -> Option<bool> {
        self.client.endpoint_present()
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlChange, MidiSession};
    use l6max_host::host_midi::consumer::Client;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Default)]
    struct FakeState {
        sent: Vec<Vec<u8>>,
        incoming: VecDeque<[u8; 3]>,
        released: bool,
    }

    struct FakeClient(Rc<RefCell<FakeState>>);
    impl Client for FakeClient {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().sent.push(bytes.to_vec());
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<[u8; 3]>, String> {
            Ok(self.0.borrow_mut().incoming.pop_front())
        }
        fn release(&mut self) {
            self.0.borrow_mut().released = true;
        }
        fn endpoint_present(&mut self) -> Option<bool> {
            Some(!self.0.borrow().released)
        }
    }

    #[test]
    fn session_retains_queue_and_endpoint_observer_after_release() {
        let state = Rc::new(RefCell::new(FakeState {
            incoming: [[0xb0, 81, 9], [0xb1, 83, 100]].into(),
            ..Default::default()
        }));
        let mut session = MidiSession {
            client: Box::new(FakeClient(state.clone())),
        };
        let first = session.poll_control_change().unwrap().unwrap();
        session.send_control_change(first).unwrap();
        assert_eq!(state.borrow().sent, vec![vec![0xb0, 81, 9]]);
        assert_eq!(session.poll_control_change().unwrap().unwrap().channel, 1);
        assert_eq!(session.poll_control_change().unwrap(), None);
        session.release();
        assert_eq!(session.endpoint_present(), Some(false));
    }

    #[test]
    fn typed_session_uses_strict_consumer_poll() {
        struct StrictClient;
        impl Client for StrictClient {
            fn send(&mut self, _: &[u8]) -> Result<(), String> {
                Ok(())
            }
            fn poll(&mut self) -> Result<Option<[u8; 3]>, String> {
                Err("legacy path called".into())
            }
            fn poll_control_change(&mut self) -> Result<Option<[u8; 3]>, String> {
                Ok(Some([0xb0, 81, 42]))
            }
        }
        let mut session = MidiSession {
            client: Box::new(StrictClient),
        };
        assert_eq!(
            session.poll_control_change().unwrap().unwrap().controller,
            81
        );
        assert_eq!(
            session.poll_control_observation().unwrap_err(),
            "legacy path called"
        );
    }

    #[test]
    fn direct_session_rejects_invalid_cable_before_usb_io() {
        let (socket, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let host = std::sync::Arc::new(l6max_host::usb::UsbHost::new(socket).unwrap());
        let error = match MidiSession::connect_direct(host, 16) {
            Ok(_) => panic!("invalid cable was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("0..=15"));
        let mut byte = [0u8; 1];
        let result = std::io::Read::read(&mut &peer, &mut byte);
        assert!(
            matches!(result, Ok(0))
                || matches!(result, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn control_change_round_trip_and_validation() {
        let message = ControlChange {
            channel: 15,
            controller: 83,
            value: 127,
        };
        assert_eq!(message.to_bytes().unwrap(), [0xbf, 83, 127]);
        assert_eq!(ControlChange::from_bytes([0xbf, 83, 127]).unwrap(), message);
        assert!(ControlChange::from_bytes([0x90, 83, 127]).is_err());
        assert!(
            ControlChange {
                channel: 16,
                ..message
            }
            .to_bytes()
            .is_err()
        );
        assert!(
            ControlChange {
                controller: 128,
                ..message
            }
            .to_bytes()
            .is_err()
        );
        assert!(
            ControlChange {
                value: 128,
                ..message
            }
            .to_bytes()
            .is_err()
        );
    }
}
