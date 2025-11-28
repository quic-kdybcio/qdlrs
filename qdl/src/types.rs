// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Qualcomm Technologies, Inc. and/or its subsidiaries.
use std::{
    fmt::Display,
    io::{BufRead, ErrorKind, Read, Write},
    str::FromStr,
};

use anyhow::{Error, bail};
use owo_colors::OwoColorize;

use crate::firehose_reset;

/// Common respones indicating success/failure respectively
#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u32)]
pub enum FirehoseStatus {
    Ack = 0,
    Nak = 1,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QdlBackend {
    Serial,
    Usb,
}

impl FromStr for QdlBackend {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "serial" => Ok(QdlBackend::Serial),
            "usb" => Ok(QdlBackend::Usb),
            _ => bail!("Unknown backend"),
        }
    }
}

impl Default for QdlBackend {
    fn default() -> Self {
        match cfg!(target_os = "windows") {
            true => QdlBackend::Serial,
            false => QdlBackend::Usb,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FirehoseConfiguration {
    // send/recv are from Host PoV
    pub send_buffer_size: usize,
    pub recv_buffer_size: usize,
    pub xml_buf_size: usize,

    pub storage_sector_size: usize,
    pub storage_type: FirehoseStorageType,

    pub bypass_storage: bool,
    pub hash_packets: bool,
    pub read_back_verify: bool,

    pub backend: QdlBackend,
    pub skip_firehose_log: bool,
    pub verbose_firehose: bool,
}

impl Default for FirehoseConfiguration {
    fn default() -> Self {
        Self {
            send_buffer_size: 1024 * 1024,
            recv_buffer_size: 4096,
            xml_buf_size: 4096,
            storage_sector_size: 512,
            storage_type: FirehoseStorageType::Emmc,
            bypass_storage: false,
            hash_packets: false,
            read_back_verify: false,
            backend: QdlBackend::default(),
            skip_firehose_log: true,
            verbose_firehose: false,
        }
    }
}
pub trait QdlChan: BufRead + Write {
    fn fh_config(&self) -> &FirehoseConfiguration;
    fn mut_fh_config(&mut self) -> &mut FirehoseConfiguration;
}

pub trait QdlReadWrite: BufRead + Write + Send + Sync {}
impl<T> QdlReadWrite for &mut T where T: QdlReadWrite + ?Sized {}

pub struct QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    pub rw: Box<T>,
    pub fh_cfg: FirehoseConfiguration,
    pub reset_on_drop: bool,
}

impl<T> Read for QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.rw.read(buf)
    }
}

impl<T> Write for QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.rw.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.rw.flush()
    }
}

impl<T> std::io::BufRead for QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.rw.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.rw.consume(amt);
    }
}

impl<T> QdlChan for QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    fn fh_config(&self) -> &FirehoseConfiguration {
        &self.fh_cfg
    }

    fn mut_fh_config(&mut self) -> &mut FirehoseConfiguration {
        &mut self.fh_cfg
    }
}

impl<T> Drop for QdlDevice<T>
where
    T: QdlReadWrite + ?Sized,
{
    fn drop(&mut self) {
        // Avoid having the board be stuck in EDL limbo in case of errors
        // TODO: watch 'rawmode' and adjust accordingly
        if self.reset_on_drop {
            println!(
                "Firehose {}. Resetting the board to {}, try again.",
                "failed".bright_red(),
                "edl".bright_yellow()
            );
            let _ = firehose_reset(self, &FirehoseResetMode::ResetToEdl, 0);
        }
    }
}

/// Supported storage media types
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FirehoseStorageType {
    Emmc,
    Ufs,
    Nand,
    Nvme,
    Spinor,
}

impl FromStr for FirehoseStorageType {
    type Err = Error;

    fn from_str(input: &str) -> Result<FirehoseStorageType, Self::Err> {
        match input {
            "emmc" => Ok(FirehoseStorageType::Emmc),
            "ufs" => Ok(FirehoseStorageType::Ufs),
            "nand" => Ok(FirehoseStorageType::Nand),
            "nvme" => Ok(FirehoseStorageType::Nvme),
            "spinor" => Ok(FirehoseStorageType::Spinor),
            _ => bail!("Unknown storage type"),
        }
    }
}

impl Display for FirehoseStorageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirehoseStorageType::Emmc => write!(f, "emmc"),
            FirehoseStorageType::Ufs => write!(f, "ufs"),
            FirehoseStorageType::Nand => write!(f, "nand"),
            FirehoseStorageType::Nvme => write!(f, "nvme"),
            FirehoseStorageType::Spinor => write!(f, "spinor"),
        }
    }
}

/// List of supported reboot modes, supplied to the \<reset\> command
pub enum FirehoseResetMode {
    ResetToEdl,
    Reset,
    Off,
}

impl FromStr for FirehoseResetMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "edl" => Ok(FirehoseResetMode::ResetToEdl),
            "system" => Ok(FirehoseResetMode::Reset),
            "off" => Ok(FirehoseResetMode::Off),
            _ => Err(std::io::Error::from(ErrorKind::InvalidInput).into()),
        }
    }
}

impl Display for FirehoseResetMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirehoseResetMode::ResetToEdl => write!(f, "edl"),
            FirehoseResetMode::Reset => write!(f, "system"),
            FirehoseResetMode::Off => write!(f, "off"),
        }
    }
}

#[derive(Debug)]
pub struct FirehoseUfsCommonConfig {
    pub num_luns: u8,

    pub boot_partition_en: bool,
    pub descr_access_en: bool,
    pub initial_power_mode: u8,
    pub high_prio_lun: u8,
    pub secure_removal_type: u8,
    pub init_active_icc_level: u8,
    pub periodic_rtc_update: u16,
    pub hpb_control: Option<u8>,
    pub config_descr_lock: bool,

    pub write_booster_buf_preserve_userspace_en: Option<u8>,
    pub write_booster_buf_type: Option<u8>,
    pub shared_wb_buffer_size_in_kb: Option<u64>,
    pub vendor_config_code: Option<u32>,
}

#[derive(Debug)]
pub struct FirehoseUfsLunConfig {
    pub lun_idx: u8,
    pub enabled: u8,
    pub use_for_boot: u8,
    pub write_protect: u8,
    pub memory_type: u8,
    pub size_in_kb: u64,
    pub reliable_writes: u8,
    pub logical_block_size: u8,
    pub provisioning_type: u8,
    pub context_capabilities: u64,
    pub wb_buffer_size_in_kb: Option<u64>,
    pub max_active_hpb_regions: Option<u16>,
    pub hpb_pinned_region_start_idx: Option<u16>,
    pub num_hpb_pinned_regions: Option<u16>,
}

#[derive(Debug)]
pub struct FirehoseUfsEpilogueConfig {
    pub lun_to_grow: Option<String>,
    pub commit: u32,
}
