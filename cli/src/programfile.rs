// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Qualcomm Technologies, Inc. and/or its subsidiaries.
use anyhow::{Context, bail};
use indexmap::IndexMap;
use std::{
    error::Error,
    fs,
    io::{Seek, SeekFrom},
    path::Path,
    str::FromStr,
};
use xmltree::{self, Element, XMLNode};

use qdl::{
    firehose_checksum_storage, firehose_patch, firehose_program_storage, firehose_read_storage,
    firehose_ufs_common, firehose_ufs_epilogue, firehose_ufs_lun,
    types::{
        FirehoseStorageType, FirehoseUfsCommonConfig, FirehoseUfsEpilogueConfig,
        FirehoseUfsLunConfig, QdlChan,
    },
};

fn parse_read_cmd<T: QdlChan>(
    channel: &mut T,
    out_dir: &Path,
    attrs: &IndexMap<String, String>,
    checksum_only: bool,
) -> anyhow::Result<()> {
    let num_sectors = attrs
        .get("num_partition_sectors")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let slot = attrs.get("slot").map_or(0, |a| a.parse::<u8>().unwrap());
    let phys_part_idx = attrs
        .get("physical_partition_number")
        .unwrap()
        .parse::<u8>()
        .unwrap();
    let start_sector = attrs.get("start_sector").unwrap().parse::<u32>().unwrap();

    if checksum_only {
        return firehose_checksum_storage(channel, num_sectors, phys_part_idx, start_sector);
    }

    if !attrs.contains_key("filename") {
        bail!("Got '<read>' tag without a filename");
    }
    let mut outfile = fs::File::create(out_dir.join(attrs.get("filename").unwrap()))?;

    firehose_read_storage(
        channel,
        &mut outfile,
        num_sectors,
        slot,
        phys_part_idx,
        start_sector,
    )
}

fn parse_patch_cmd<T: QdlChan>(
    channel: &mut T,
    attrs: &IndexMap<String, String>,
    verbose: bool,
) -> anyhow::Result<()> {
    if let Some(filename) = attrs.get("filename") {
        if filename != "DISK" {
            if verbose {
                println!("Skipping <patch> tag trying to alter {filename} on Host filesystem");
            }
            return Ok(());
        }
    } else {
        bail!("Got '<patch>' tag without a filename");
    }

    let byte_off = attrs.get("byte_offset").unwrap().parse::<u64>().unwrap();
    let slot = attrs.get("slot").map_or(0, |a| a.parse::<u8>().unwrap());
    let phys_part_idx = attrs
        .get("physical_partition_number")
        .unwrap()
        .parse::<u8>()
        .unwrap();
    let size = attrs.get("size_in_bytes").unwrap().parse::<u64>().unwrap();
    let start_sector = attrs.get("start_sector").unwrap();
    let val = attrs.get("value").unwrap();

    firehose_patch(
        channel,
        byte_off,
        slot,
        phys_part_idx,
        size,
        start_sector,
        val,
    )
}

const BOOTABLE_PART_NAMES: [&str; 3] = ["xbl", "xbl_a", "sbl1"];

// TODO: readbackverify
fn parse_program_cmd<T: QdlChan>(
    channel: &mut T,
    program_file_dir: &Path,
    attrs: &IndexMap<String, String>,
    allow_missing_files: bool,
    bootable_part_idx: &mut Option<u8>,
    verbose: bool,
) -> anyhow::Result<()> {
    let sector_size = attrs
        .get("SECTOR_SIZE_IN_BYTES")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    if sector_size != channel.fh_config().storage_sector_size {
        bail!(
            "Mismatch in storage sector size! Programfile requests {}",
            sector_size
        );
    }
    let num_sectors = attrs
        .get("num_partition_sectors")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let slot = attrs.get("slot").map_or(0, |a| a.parse::<u8>().unwrap());
    let phys_part_idx = attrs
        .get("physical_partition_number")
        .unwrap()
        .parse::<u8>()
        .unwrap();
    let start_sector = attrs.get("start_sector").unwrap();
    let file_sector_offset = attrs
        .get("file_sector_offset")
        .unwrap_or(&"".to_owned())
        .parse::<u32>()
        .unwrap_or(0);

    let label = attrs.get("label").unwrap();
    if num_sectors == 0 {
        println!("Skipping 0-length entry for {label}");
        return Ok(());
    }
    if BOOTABLE_PART_NAMES.contains(&&label[..]) {
        *bootable_part_idx = Some(phys_part_idx);
    }

    let filename = attrs.get("filename").unwrap();
    let file_path = program_file_dir.join(filename);
    if allow_missing_files {
        if filename.is_empty() {
            if verbose {
                println!("Skipping bogus entry for {label}");
            }
            return Ok(());
        } else if !file_path.exists() {
            if verbose {
                println!("Skipping non-existent file {}", file_path.to_str().unwrap());
            }
            return Ok(());
        }
    }

    let mut buf = fs::File::open(file_path)?;
    buf.seek(SeekFrom::Current(
        sector_size as i64 * file_sector_offset as i64,
    ))?;

    firehose_program_storage(
        channel,
        &mut buf,
        label,
        num_sectors,
        slot,
        phys_part_idx,
        start_sector,
    )
}

fn programfile_parse_value<T: FromStr>(
    attrs: &IndexMap<String, String>,
    key: &str,
) -> anyhow::Result<T>
where
    <T as FromStr>::Err: Error + Send + Sync + 'static,
{
    let a = attrs.get(key);

    a.unwrap()
        .parse::<T>()
        .context(format!("Couldn't parse {key}"))
}

fn programfile_parse_value_opt<T: FromStr>(
    attrs: &IndexMap<String, String>,
    key: &str,
) -> anyhow::Result<Option<T>>
where
    <T as FromStr>::Err: Error + Send + Sync + 'static,
{
    let a = attrs.get(key);

    match a.is_none() {
        true => Ok(Some(
            a.unwrap()
                .parse::<T>()
                .context(format!("Couldn't parse {key}"))?,
        )),
        false => Ok(None),
    }
}

fn parse_ufs_common_cmd<T: QdlChan>(
    channel: &mut T,
    attrs: &IndexMap<String, String>,
    allow_final_provisioning: bool,
) -> anyhow::Result<()> {
    let cfg = FirehoseUfsCommonConfig {
        // This param isn't interpreted nowadays
        num_luns: programfile_parse_value::<u8>(attrs, "bNumberLU")?,
        boot_partition_en: programfile_parse_value::<u8>(attrs, "bBootEnable")? != 0,
        descr_access_en: programfile_parse_value::<u8>(attrs, "bDescrAccessEn")? != 0,
        initial_power_mode: programfile_parse_value::<u8>(attrs, "bInitPowerMode")?,
        high_prio_lun: programfile_parse_value::<u8>(attrs, "bHighPriorityLUN")?,
        secure_removal_type: programfile_parse_value::<u8>(attrs, "bSecureRemovalType")?,
        init_active_icc_level: programfile_parse_value::<u8>(attrs, "bInitActiveICCLevel")?,
        periodic_rtc_update: programfile_parse_value::<u16>(attrs, "wPeriodicRTCUpdate")?,
        config_descr_lock: programfile_parse_value::<u8>(attrs, "bConfigDescrLock")? != 0,

        hpb_control: programfile_parse_value_opt::<u8>(attrs, "bHPBControl")?,
        write_booster_buf_preserve_userspace_en: programfile_parse_value_opt(
            attrs,
            "bWriteBoosterBufferPreserveUserSpaceEn",
        )?,
        write_booster_buf_type: programfile_parse_value_opt(attrs, "bWriteBoosterBufferType")?,
        shared_wb_buffer_size_in_kb: programfile_parse_value_opt(
            attrs,
            "shared_wb_buffer_size_in_kb",
        )?,
        vendor_config_code: programfile_parse_value_opt(attrs, "qVendorConfigCode")?,
    };

    firehose_ufs_common(channel, cfg, false)
}

fn parse_ufs_lun_cmd<T: QdlChan>(
    channel: &mut T,
    attrs: &IndexMap<String, String>,
) -> anyhow::Result<()> {
    let cfg = FirehoseUfsLunConfig {
        lun_idx: programfile_parse_value::<u8>(attrs, "LUNum")?,
        enabled: programfile_parse_value::<u8>(attrs, "bLUEnable")?,
        use_for_boot: programfile_parse_value::<u8>(attrs, "bBootLunID")?,
        write_protect: programfile_parse_value::<u8>(attrs, "bLUWriteProtect")?,
        memory_type: programfile_parse_value::<u8>(attrs, "bMemoryType")?,
        size_in_kb: programfile_parse_value::<u64>(attrs, "size_in_kb")?,
        reliable_writes: programfile_parse_value::<u8>(attrs, "bDataReliability")?,
        logical_block_size: programfile_parse_value::<u8>(attrs, "bLogicalBlockSize")?,
        provisioning_type: programfile_parse_value::<u8>(attrs, "bProvisioningType")?,
        context_capabilities: programfile_parse_value::<u64>(attrs, "wContextCapabilities")?
            & 0xffff,
        wb_buffer_size_in_kb: programfile_parse_value_opt::<u64>(attrs, "wb_buffer_size_in_kb")?,
        max_active_hpb_regions: programfile_parse_value_opt::<u16>(
            attrs,
            "wLUMaxActiveHPBRegions",
        )?,
        hpb_pinned_region_start_idx: programfile_parse_value_opt::<u16>(
            attrs,
            "wHPBPinnedRegionStartIdx",
        )?,
        num_hpb_pinned_regions: programfile_parse_value_opt::<u16>(attrs, "wNumHPBPinnedRegions")?,
    };

    firehose_ufs_lun(channel, cfg)
}

fn parse_ufs_cmd<T: QdlChan>(
    channel: &mut T,
    attrs: &IndexMap<String, String>,
    allow_final_provisioning: bool,
) -> anyhow::Result<()> {
    if channel.fh_config().storage_type != FirehoseStorageType::Ufs {};

    if attrs.contains_key("LUNum") {
        parse_ufs_lun_cmd(channel, attrs)?;
    } else if let Some(commit) = programfile_parse_value_opt(attrs, "commit")? {
        let cfg = FirehoseUfsEpilogueConfig {
            commit,
            lun_to_grow: attrs.get("LUNtoGrow").cloned(),
        };

        firehose_ufs_epilogue(channel, cfg)?;
    } else {
        parse_ufs_common_cmd(channel, attrs, false)?;
    }

    Ok(())
}

// TODO: there's some funny optimizations to make here, such as OoO loading files into memory, or doing things while we're waiting on the device to finish
pub fn parse_program_xml<T: QdlChan>(
    channel: &mut T,
    xml: &Element,
    program_file_dir: &Path,
    out_dir: &Path,
    allow_missing_files: bool,
    verbose: bool,
) -> anyhow::Result<Option<u8>> {
    let mut bootable_part_idx: Option<u8> = None;

    // First make sure we have all the necessary files (and fail unless specified otherwise)
    for node in xml.children.iter() {
        if let XMLNode::Element(e) = node {
            match e.name.to_lowercase().as_str() {
                "program" => {
                    if !e.attributes.contains_key("filename") {
                        bail!("Got '<program>' tag without a filename");
                    }

                    let filename = e.attributes.get("filename").unwrap();
                    let file_path = program_file_dir.join(filename);

                    if !file_path.exists() && !allow_missing_files {
                        bail!("{} doesn't exist!", file_path.to_str().unwrap())
                    }
                }
                _ => continue,
            }
        }
    }

    // At last, do the things we're supposed to do
    for node in xml.children.iter() {
        if let XMLNode::Element(e) = node {
            match e.name.to_lowercase().as_str() {
                "getsha256digest" => parse_read_cmd(channel, out_dir, &e.attributes, true)?,
                "patch" => parse_patch_cmd(channel, &e.attributes, verbose)?,
                "program" => parse_program_cmd(
                    channel,
                    program_file_dir,
                    &e.attributes,
                    allow_missing_files,
                    &mut bootable_part_idx,
                    verbose,
                )?,
                "read" => parse_read_cmd(channel, out_dir, &e.attributes, false)?,
                "ufs" => parse_ufs_cmd(channel, &e.attributes, false)?,
                unknown => bail!(
                    "Got unknown instruction ({}), failing to prevent damage",
                    unknown
                ),
            };
        }
    }

    Ok(bootable_part_idx)
}
