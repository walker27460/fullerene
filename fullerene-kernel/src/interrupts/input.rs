//! Input device interrupt handlers
//!
//! This module handles keyboard and mouse interrupts.

use super::apic::send_eoi;
use petroleum::port_read_u8;
use x86_64::structures::idt::InterruptStackFrame;

/// Macro to create input device interrupt handlers
macro_rules! define_input_interrupt_handler {
    ($handler_name:ident, $port:expr, $process_input:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $handler_name(_stack_frame: InterruptStackFrame) {
            let data = port_read_u8!($port);
            $process_input(data);
            send_eoi();
        }
    };
}

// Keyboard interrupt handler — stub
// PS/2 keyboard driver was removed from nitrogen.
define_input_interrupt_handler!(keyboard_handler, 0x60, |_scancode: u8| {
    // no-op — PS/2 keyboard driver removed
});

// Mouse interrupt handler — stub
// PS/2 mouse driver was removed from nitrogen.
define_input_interrupt_handler!(mouse_handler, 0x60, |_byte: u8| {
    // no-op — PS/2 mouse driver removed
});

/// Timer interrupt handler (no preemption - scheduler loop handles yielding)
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Increment global tick counter (lock-free atomic increment)
    super::TICK_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    send_eoi();
}
