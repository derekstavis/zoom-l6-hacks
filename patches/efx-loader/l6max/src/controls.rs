//! Knob editing for replacement effects, shared by every effect package.
//!
//! An effect describes its parameters in a table and supplies a few
//! functions to read and apply them; stepping, clamping, ring positions, the
//! Knob Dialog text and the remembered values live here once. Effect modules
//! are always composed with efx-loader, which routes the stock effect slots
//! to them, so a composed module carries one copy of this code however many
//! effects it holds.
use core::sync::atomic::{AtomicU32, Ordering};

/// Full scale of an encoder ring position, in Q16.
const RING_FULL: f32 = 44.0 * 65536.0;
const NOT_SAVED: u32 = u32::MAX;

/// One knob-editable effect parameter.
pub struct Parameter {
    /// The Knob Dialog title while the parameter is edited.
    pub title: &'static [u8],
    pub kind: Kind,
}

pub enum Kind {
    /// A number stepped by `step` per encoder detent within `min..=max`, shown
    /// as `value * scale` with `unit`. The ring spans `ring_min..=ring_max` in
    /// the shown units.
    Number {
        step: f32,
        min: f32,
        max: f32,
        scale: f32,
        unit: &'static [u8],
        ring_min: f32,
        ring_max: f32,
    },
    /// Turning right sets it, turning left clears it.
    Toggle {
        on: &'static [u8],
        off: &'static [u8],
    },
}

/// Everything the shared editing needs from one effect.
pub struct Effect {
    pub parameters: &'static [Parameter],
    /// The Knob Dialog title when the editor turns on or off.
    pub editor_title: &'static [u8],
    /// The last edited value of each parameter, restored at initialization.
    pub saved: &'static [AtomicU32],
    /// The current value of parameter `index`; a toggle reads 1.0 when set.
    pub value: unsafe fn(state: *mut u8, index: usize) -> f32,
    /// Apply `value` to parameter `index`.
    pub apply: unsafe fn(state: *mut u8, index: usize, value: f32),
    /// A maximum that depends on other parameters, replacing both `max` and
    /// `ring_max`.
    pub dynamic_max: unsafe fn(state: *mut u8, index: usize) -> Option<f32>,
    /// A shown value that depends on the processor, replacing
    /// `value * scale`.
    pub shown: unsafe fn(state: *mut u8, index: usize, value: f32) -> Option<f32>,
}

/// For effects with no dependent maxima or shown values.
pub unsafe fn none(_: *mut u8, _: usize) -> Option<f32> {
    None
}

/// For effects whose shown values are always `value * scale`.
pub unsafe fn scaled(_: *mut u8, _: usize, _: f32) -> Option<f32> {
    None
}

impl Effect {
    /// The remembered value of parameter `index`, if it was ever edited.
    pub fn saved(&self, index: usize) -> Option<f32> {
        let bits = self.saved[index].load(Ordering::Acquire);
        (bits != NOT_SAVED).then(|| f32::from_bits(bits))
    }

    /// The ring position of the current value.
    pub unsafe fn ring_position(&self, state: *mut u8, index: usize) -> Option<u32> {
        if index >= self.parameters.len() {
            return None;
        }
        let value = unsafe { (self.value)(state, index) };
        unsafe { self.position(state, index, value) }
    }

    /// The ring position one edit of `delta` would reach, without applying it.
    pub unsafe fn preview_ring_position(
        &self,
        state: *mut u8,
        index: usize,
        delta: i32,
    ) -> Option<u32> {
        if delta == 0 || index >= self.parameters.len() {
            return None;
        }
        let value = unsafe { self.stepped(state, index, delta) };
        unsafe { self.position(state, index, value) }
    }

    /// Apply one edit of `delta`, remember the result, show it in the Knob
    /// Dialog and return the new ring position.
    pub unsafe fn edit(&self, state: *mut u8, index: usize, delta: i32) -> Option<u32> {
        if delta == 0 || index >= self.parameters.len() {
            return None;
        }
        let value = unsafe { self.stepped(state, index, delta) };
        unsafe { (self.apply)(state, index, value) };
        let applied = unsafe { (self.value)(state, index) };
        self.saved[index].store(applied.to_bits(), Ordering::Release);
        let parameter = &self.parameters[index];
        match parameter.kind {
            Kind::Number { scale, unit, .. } => {
                let shown = unsafe { (self.shown)(state, index, applied) }
                    .unwrap_or(applied * scale);
                let (text, len) = format_unit(shown, unit);
                show_external_text(parameter.title, &text[..len]);
            }
            Kind::Toggle { on, off } => {
                show_external_text(parameter.title, if applied != 0.0 { on } else { off })
            }
        }
        unsafe { self.ring_position(state, index) }
    }

    pub fn show_editor_state(&self, enabled: bool) {
        show_external_text(self.editor_title, if enabled { b"ON" } else { b"OFF" });
    }

    unsafe fn stepped(&self, state: *mut u8, index: usize, delta: i32) -> f32 {
        match self.parameters[index].kind {
            Kind::Number { step, min, max, .. } => {
                let max = unsafe { (self.dynamic_max)(state, index) }.unwrap_or(max);
                let current = unsafe { (self.value)(state, index) };
                clamp(current + delta as f32 * step, min, max)
            }
            Kind::Toggle { .. } => {
                if delta > 0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    unsafe fn position(&self, state: *mut u8, index: usize, value: f32) -> Option<u32> {
        match self.parameters[index].kind {
            Kind::Number {
                scale,
                ring_min,
                ring_max,
                ..
            } => {
                let shown =
                    unsafe { (self.shown)(state, index, value) }.unwrap_or(value * scale);
                let ring_max = unsafe { (self.dynamic_max)(state, index) }.unwrap_or(ring_max);
                Some(ring_q16(shown, ring_min, ring_max))
            }
            Kind::Toggle { .. } => Some(if value != 0.0 { 44 << 16 } else { 0 }),
        }
    }
}

fn show_external_text(title: &[u8], value: &[u8]) {
    let _ = crate::knob_dialog::prepare_external_text();
    let _ = crate::knob_dialog::show_external_text(title, value);
}

fn ring_q16(value: f32, min: f32, max: f32) -> u32 {
    (clamp((value - min) / (max - min), 0.0, 1.0) * RING_FULL + 0.5) as u32
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// `value` with one decimal, followed by `unit`.
fn format_unit(value: f32, unit: &[u8]) -> ([u8; 16], usize) {
    let mut out = [0; 16];
    let mut len = 0;
    let scaled = (value.abs() * 10.0 + 0.5) as u32;
    if value < 0.0 {
        out[len] = b'-';
        len += 1;
    }
    len += push_integer(&mut out[len..], scaled / 10);
    out[len] = b'.';
    len += 1;
    out[len] = b'0' + (scaled % 10) as u8;
    len += 1;
    let copy_len = unit.len().min(out.len() - len);
    for (slot, byte) in out[len..].iter_mut().zip(unit) {
        *slot = *byte;
    }
    len += copy_len;
    (out, len)
}

fn push_integer(out: &mut [u8], mut value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut count = 0;
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
    }
    for (target, digit) in out.iter_mut().zip(digits[..count].iter().rev()) {
        *target = *digit;
    }
    count
}
