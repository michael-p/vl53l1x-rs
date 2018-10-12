extern crate byteorder;
use byteorder::{BigEndian, ByteOrder};
extern crate i2cdev;
use i2cdev::core::*;
use i2cdev::linux::{LinuxI2CDevice, LinuxI2CError};
use std::thread;
use std::time;

pub fn get_i2c_bus_path(i2c_bus: i32) -> String {
    format!("/dev/i2c-{}", i2c_bus)
}

pub const VL53L1_I2C_ADDR: u16 = 0x29;

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Vl53l1xReg {
    IdentificationModelId = 0x010f,
    SoftReset = 0x0000,
    FirmwareSystemStatus = 0x00e5,
    PadI2cHvExtsupConfig = 0x002e,
    GpioTioHvStatus = 0x0031,
    ResultFinalCrosstalkCorrectedRangeMmSd0 = 0x0096,
    ResultPeakSignalCountRateCrosstalkCorrectedMcpsSd0 = 0x0098,
    RangeConfigVcselPeriodA = 0x0060,
    RangeConfigVcselPeriodB = 0x0063,
    RangeConfigValidPhaseHigh = 0x0069,
    RoiConfigUserRoiCentreSpad = 0x007f,
    RoiConfigUserRoiRequestedGlobalXySize = 0x0080,
    SdConfigWoiSd0 = 0x0078,
    SdConfigWoiSd1 = 0x0079,
    SdConfigInitialPhaseSd0 = 0x007a,
    SdConfigInitialPhaseSd1 = 0x007b,
}

impl Vl53l1xReg {
    fn addr(&self) -> u16 {
        *self as u16
    }
}

pub struct Vl53l1x {
    i2c_dev: LinuxI2CDevice,
    config: Vec<u8>,
    /// The distance correction factor. The device has a dead zone between the
    /// VCSEL and SPAD so when the device reports 1mm, it's actually 1mm from
    /// the deadzone, which I have empirically found to be around 13cm. This
    /// differs per device and things like reflow even affect it. It's
    /// important to determine the offset by testing with a known distance and
    /// comparing against samples while offset is set to 0.
    range_offset: u16,
}

const MODEL_ID: u16 = 0xeacc;

impl Vl53l1x {

    /// Connects to VL53L1X.
    ///
    /// If i2c_addr is None, defaults to 0x29.
    pub fn new(i2c_bus: i32, i2c_addr: Option<u16>, range_offset: u16)
               -> Result<Vl53l1x, LinuxI2CError> {
        let i2c_dev = LinuxI2CDevice::new(
            get_i2c_bus_path(i2c_bus), i2c_addr.unwrap_or(0x29))?;

        let mut vl = Vl53l1x {
            i2c_dev,
            config: CONFIG.to_vec(),
            range_offset,
        };

        vl.check_model_id()?;
        vl.soft_reset()?;
        vl.wait_for_firmware_status_ready()?;
        vl.write_i2c_28v()?;
        vl.get_trim_resistors()?;

        return Ok(vl);
    }

    fn check_model_id(&mut self) -> Result<(), LinuxI2CError> {
        self.i2c_dev.write(
            &addr_to_bytes(Vl53l1xReg::IdentificationModelId.addr()))?;

        let mut buf2 = [0u8, 2];
        self.i2c_dev.read(&mut buf2)?;
        let test_model_id = BigEndian::read_u16(&buf2);

        if test_model_id != MODEL_ID {
            panic!("Unexpected model_id: {} != {}", test_model_id, MODEL_ID);
        }

        Ok(())
    }

    fn soft_reset(&mut self) -> Result<(), LinuxI2CError> {
        self.i2c_dev.write(&[
            (Vl53l1xReg::SoftReset.addr() >> 8) as u8,
            (Vl53l1xReg::SoftReset.addr() & 0xff) as u8,
            0x00,
        ])?;

        thread::sleep(time::Duration::from_micros(100));

        self.i2c_dev.write(&[
            (Vl53l1xReg::SoftReset.addr() >> 8) as u8,
            (Vl53l1xReg::SoftReset.addr() & 0xff) as u8,
            0x01,
        ])?;

        thread::sleep(time::Duration::from_micros(200));

        Ok(())
    }

    fn wait_for_firmware_status_ready(&mut self) -> Result<(), LinuxI2CError> {
        let mut attempts = 0;
        let mut buf2 = [0u8, 2];
        loop {
            self.i2c_dev.write(
            &addr_to_bytes(Vl53l1xReg::FirmwareSystemStatus.addr()))?;
                self.i2c_dev.read(&mut buf2)?;
            let system_status = BigEndian::read_u16(&buf2);
            if system_status & 0x0001 != 0 {
                attempts += 1;
                if attempts > 100 {
                    // TODO: Change to something recoverable.
                    panic!("Sensor timed out.")
                }
                thread::sleep(time::Duration::from_millis(10));
            } else {
                break;
            }
        }

        Ok(())
    }

    fn write_i2c_28v(&mut self) -> Result<(), LinuxI2CError> {
        let mut reg_value = i2c_read_u16(
            &mut self.i2c_dev, Vl53l1xReg::PadI2cHvExtsupConfig.addr())?;
        reg_value = (reg_value & 0xfe) | 0x01;

        i2c_write_u16(
            &mut self.i2c_dev, Vl53l1xReg::PadI2cHvExtsupConfig.addr(), reg_value)?;

        Ok(())
    }

    fn get_trim_resistors(&mut self) -> Result<(), LinuxI2CError> {
        for i in 0..36 {
            self.config[i] = i2c_read_u8(&mut self.i2c_dev, (i + 1) as u16)?;
        }
        Ok(())
    }

    pub fn start_measurement(&mut self) -> Result<(), LinuxI2CError> {
        let mut config = self.config.clone();
        // Add address (0x0001)
        config.insert(0, 0x01);
        config.insert(0, 0x00);
        self.i2c_dev.write(&config)?;
        Ok(())
    }

    pub fn check_data_ready(&mut self) -> Result<bool, LinuxI2CError> {
        let val = i2c_read_u8(
            &mut self.i2c_dev, Vl53l1xReg::GpioTioHvStatus.addr())?;
        Ok(val != 0x03)
    }

    pub fn wait_data_ready(&mut self) -> Result<(), LinuxI2CError> {
        while !self.check_data_ready()? {
            thread::sleep(time::Duration::from_millis(5));
        }
        Ok(())
    }

    pub fn read_distance(&mut self) -> Result<u16, LinuxI2CError> {
        let val = i2c_read_u16(
            &mut self.i2c_dev,
            Vl53l1xReg::ResultFinalCrosstalkCorrectedRangeMmSd0.addr())?;
        Ok(val)
    }

    pub fn read_signal_rate(&mut self) -> Result<u16, LinuxI2CError> {
        let val = i2c_read_u16(
            &mut self.i2c_dev,
            Vl53l1xReg::ResultPeakSignalCountRateCrosstalkCorrectedMcpsSd0.addr())?;
        Ok(val)
    }

    pub fn read_sample(&mut self) -> Result<Vl53l1xSample, LinuxI2CError> {
        self.i2c_dev.write(
            &addr_to_bytes(Vl53l1xReg::ResultFinalCrosstalkCorrectedRangeMmSd0.addr()))?;
        let mut buf4 = [0u8; 6];
        self.i2c_dev.read(&mut buf4)?;

        let distance = BigEndian::read_u16(&buf4[0 .. 2]);
        let signal_rate = BigEndian::read_u16(&buf4[2 .. 4]);

        let corrected;
        if distance == 0 && signal_rate > 20000 {
            corrected = Vl53l1xCorrectedSample::TooClose;
        } else if signal_rate < 100 {
            corrected = Vl53l1xCorrectedSample::TooFar;
        } else {
            corrected = Vl53l1xCorrectedSample::Ok(distance + self.range_offset);
        }

        Ok(Vl53l1xSample {
            distance,
            signal_rate,
            corrected,
        })
    }

    pub fn write_distance_mode(&mut self, mode: DistanceMode) -> Result<(), LinuxI2CError> {
        let period_a;
        let period_b;
        let phase_high;
        let phase_init;

        match mode {
            DistanceMode::Short => {
                period_a = 0x07;
                period_b = 0x05;
                phase_high = 0x38;
                phase_init = 6;
            },
            DistanceMode::Mid => {
                period_a = 0x0f;
                period_b = 0x0d;
                phase_high = 0xb8;
                phase_init = 14;
            },
            DistanceMode::Long => {
                period_a = 0x0f;
                period_b = 0x0d;
                phase_high = 0xb8;
                phase_init = 14;
            }
        }

        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::RangeConfigVcselPeriodA.addr(), period_a)?;
        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::RangeConfigVcselPeriodB.addr(), period_b)?;
        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::RangeConfigValidPhaseHigh.addr(), phase_high)?;

        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::SdConfigWoiSd0.addr(), period_a)?;
        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::SdConfigWoiSd1.addr(), period_b)?;
        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::SdConfigInitialPhaseSd0.addr(), phase_init)?;
        i2c_write_u8(
            &mut self.i2c_dev, Vl53l1xReg::SdConfigInitialPhaseSd1.addr(), phase_init)?;

        Ok(())
    }

    /// Returns (TopLeftX, TopLeftY, BotRightX, BotRightY)
    pub fn get_user_roi(&mut self) -> Result<(u8, u8, u8, u8), LinuxI2CError> {

        let center = i2c_read_u8(
            &mut self.i2c_dev, Vl53l1xReg::RoiConfigUserRoiCentreSpad.addr())?;
        let row;
        let col;
        if center > 127 {
            row = 8 + ((255 - center) & 0x07);
            col = (center - 128) >> 3;
        } else {
            row = center & 0x07;
            col = (127 - center) >> 3;
        }

        let dimensions = i2c_read_u8(
            &mut self.i2c_dev,
            Vl53l1xReg::RoiConfigUserRoiRequestedGlobalXySize.addr())?;
        let height = dimensions >> 4;
        let width = dimensions & 0x0f;

        Ok((
            (2 * col - width) >> 1,
            (2 * row - height) >> 1,
            (2 * col + width) >> 1,
            (2 * row + height) >> 1,
            ))
    }

    pub fn set_zone_size(&mut self, width: u8, height: u8) -> Result<(), LinuxI2CError> {
        let dimensions = (height << 4) + width;
        i2c_write_u8(
            &mut self.i2c_dev,
            Vl53l1xReg::RoiConfigUserRoiRequestedGlobalXySize.addr(), dimensions)?;
        Ok(())
    }

    pub fn set_center(&mut self, center_x: u8, center_y: u8) -> Result<(), LinuxI2CError> {
        let center;
        if center_y > 7 {
            center = 128 + (center_x << 3) + (15 - center_y);
        } else {
            center = ((15 - center_x) << 3) + center_y;
        }
        i2c_write_u8(
            &mut self.i2c_dev,
            Vl53l1xReg::RoiConfigUserRoiCentreSpad.addr(), center)?;
        Ok(())
    }

    /// Setting the ROI changes the sensor's diagonal field-of-view.
    /// 16x16: 27 degrees, 8x8: 20 degrees, 4x4, 15 degrees
    /// roi: (topLeftX, topLeftY, bottomRightX, bottomRightY)
    pub fn set_user_roi(&mut self, roi: (u8, u8, u8, u8)) -> Result<(), LinuxI2CError> {
        let center_x = (roi.0 + roi.2 + 1) / 2;
        let center_y = (roi.1 + roi.3 + 1) / 2;
        let width = roi.2 - roi.0;
        let height = roi.3 - roi.1;

        if width < 3 || height < 3 {
            panic!("Width and height must be at least 4.")
        } else {
            self.set_center(center_x, center_y)?;
            self.set_zone_size(width, height)?;
        }
        Ok(())
    }
}

pub enum DistanceMode {
    /// Max distance: 1360mm (dark), 1350mm (ambient)
    Short,
    /// Max distance: 2900mm (dark), 760mm (ambient)
    Mid,
    /// Max distance: 3600mm (dark), 730mm (ambient)
    Long,
}

#[derive(Debug)]
pub struct Vl53l1xSample {
    /// Distance is in mm.
    pub distance: u16,
    /// Empirically, ranges between 0 to 40,000.
    pub signal_rate: u16,
    pub corrected: Vl53l1xCorrectedSample,
}

#[derive(Debug)]
pub enum Vl53l1xCorrectedSample {
    TooClose,
    Ok(u16),
    TooFar,
}

fn addr_to_bytes(addr: u16) -> [u8; 2] {
    [
        (addr >> 8) as u8,
        (addr & 0xff) as u8,
    ]
}

fn i2c_read_u8(i2c_dev: &mut LinuxI2CDevice, addr: u16) -> Result<u8, LinuxI2CError> {
    i2c_dev.write(
        &addr_to_bytes(addr))?;
    let mut buf1 = [0u8, 1];
    i2c_dev.read(&mut buf1)?;

    Ok(buf1[0])
}

fn i2c_read_u16(i2c_dev: &mut LinuxI2CDevice, addr: u16) -> Result<u16, LinuxI2CError> {
    i2c_dev.write(
        &addr_to_bytes(addr))?;
    let mut buf2 = [0u8, 2];
    i2c_dev.read(&mut buf2)?;

    Ok(BigEndian::read_u16(&buf2))
}

fn i2c_write_u8(i2c_dev: &mut LinuxI2CDevice, addr: u16, val: u8) -> Result<(), LinuxI2CError> {
    i2c_dev.write(&[
        (addr >> 8) as u8,
        (addr & 0xff) as u8,
        val,
    ])?;

    Ok(())
}

fn i2c_write_u16(i2c_dev: &mut LinuxI2CDevice, addr: u16, val: u16) -> Result<(), LinuxI2CError> {
    i2c_dev.write(&[
        (addr >> 8) as u8,
        (addr & 0xff) as u8,
        (val >> 8) as u8,
        (val & 0xff) as u8,
    ])?;

    Ok(())
}

const CONFIG: [u8; 135] = [
  0x29, 0x02, 0x10, 0x00, 0x28, 0xBC, 0x7A, 0x81,
  0x80, 0x07, 0x95, 0x00, 0xED, 0xFF, 0xF7, 0xFD,
  0x9E, 0x0E, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x34, 0x00,
  0x28, 0x00, 0x0D, 0x0A, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11,
  0x02, 0x00, 0x02, 0x08, 0x00, 0x08, 0x10, 0x01,
  0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x02,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x0B, 0x00,
  0x00, 0x02, 0x0A, 0x21, 0x00, 0x00, 0x02, 0x00,
  0x00, 0x00, 0x00, 0xC8, 0x00, 0x00, 0x38, 0xFF,
  0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x91, 0x0F,
  0x00, 0xA5, 0x0D, 0x00, 0x80, 0x00, 0x0C, 0x08,
  0xB8, 0x00, 0x00, 0x00, 0x00, 0x0E, 0x10, 0x00,
  0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0F,
  0x0D, 0x0E, 0x0E, 0x01, 0x00, 0x02, 0xC7, 0xFF,
  0x8B, 0x00, 0x00, 0x00, 0x01, 0x01, 0x40,
];
