#![no_std]
#![no_main]

use core::fmt::Write;
use cortex_m_rt::entry;
use panic_halt as _;
use smt32f4xx_hal::{
    i2c::Mode as I2cMode,
    can::Mode as CanMode,
    spi::{self, Mode as SpiMode, Phase, Polarity},
    usb::Mode as UsbMode,
    gpio::*,
    pac::{self},
    prelude::*,
    serial::{config::Config, Mode as SerialMode}
};

#[entry]
fn main() -> !{

}