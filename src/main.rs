#![allow(non_upper_case_globals, non_snake_case, non_camel_case_types)]

use clap::Parser;
use md5::digest::DynDigest;
use sha2::Digest;
use std::collections::HashMap;

use e01::e01_reader::{E01Error, E01Reader};

#[derive(Clone, Debug, clap::ValueEnum, PartialEq)]
pub enum AddDigest {
    md5,
    sha1,
    sha256,
    sha512,
}

impl From<&AddDigest> for &str {
    fn from(v: &AddDigest) -> Self {
        match *v {
            AddDigest::md5 => "MD5",
            AddDigest::sha1 => "SHA1",
            AddDigest::sha256 => "SHA256",
            AddDigest::sha512 => "SHA512",
        }
    }
}

impl From<&AddDigest> for String {
    fn from(v: &AddDigest) -> Self {
        let s: &str = v.into();
        s.to_string()
    }
}

#[derive(Parser)]
#[command(author, version, about, long_about)]
struct Cli {
    /// Path to input file.
    input: String,

    /// Calculate additional digest (hash) types besides md5, options: sha256, sha512
    #[arg(short = 'd', long = "digest", value_enum, name = "hash")]
    additional_digest: Option<AddDigest>,

    /// ignore all checksums during read, default value is false
    #[arg(short, long, default_value = "false")]
    ignore_checksums: bool,
}

use std::process::ExitCode;

fn check_hash(
    result: &Option<bool>,
    stored: &str,
    calc: &str,
) -> Option<bool> {
    if let Some(r) = result {
        if *r == false {
            return Some(false);
        }
    }
    return Some(stored == calc);
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut stored_md5 = None;
    let mut stored_sha1 = None;

    let hashes = dump(
        &cli.input,
        &cli.additional_digest,
        &mut stored_md5,
        &mut stored_sha1,
        cli.ignore_checksums,
    )
    .unwrap();

    let mut result = None;

    if cli.additional_digest.is_none() {
        if stored_md5.is_some() {
            let ad_str: String = (&AddDigest::md5).into();
            let calc_hash = hashes.get(&ad_str).unwrap();
            if let Some(md5) = stored_md5 {
                println!("MD5 hash stored in file:       {}", md5);
                result = check_hash(&result, &md5, &calc_hash);
            }
            else {
                println!("MD5 hash stored in file:       N/A");
            }
            println!("MD5 hash calculated over data: {}", calc_hash);
        }
        if stored_sha1.is_some() {
            let ad_str: String = (&AddDigest::sha1).into();
            let calc_hash = hashes.get(&ad_str).unwrap();
            if let Some(sha1) = stored_sha1 {
                println!("SHA1 hash stored in file:       {}", sha1);
                result = check_hash(&result, &sha1, &calc_hash);
            }
            else {
                println!("SHA1 hash stored in file:       N/A");
            }
            println!("SHA1 hash calculated over data: {}", calc_hash);
        }
    }
    else if let Some(ad) = &cli.additional_digest {
        let ad_str: String = ad.into();
        let calc_hash = hashes.get(&ad_str).unwrap();
        if *ad == AddDigest::md5 {
            if let Some(md5) = &stored_md5 {
                println!("MD5 hash stored in file:       {}", md5);
                result = Some(md5 == calc_hash);
                result = check_hash(&result, &md5, &calc_hash);
            }
            else {
                println!("MD5 hash stored in file:       N/A");
            }
            println!("MD5 hash calculated over data: {}", calc_hash);
        }
        else if *ad == AddDigest::sha1 {
            if let Some(sha1) = &stored_sha1 {
                println!("SHA1 hash stored in file:       {}", sha1);
                result = check_hash(&result, &sha1, &calc_hash);
            }
            else {
                println!("SHA1 hash stored in file:       N/A");
            }
            println!("SHA1 hash calculated over data: {}", calc_hash);
        }
        else {
            println!("{} hash stored in file:       N/A", ad_str);
            println!("{} hash calculated over data: {}", ad_str, calc_hash);
        }

        if stored_md5.is_some() || stored_sha1.is_some() {
            println!("\nAdditional hash values:");
            if *ad != AddDigest::md5 {
                if let Some(md5) = stored_md5 {
                    println!("MD5:  {}", md5);
                }
            }
            if *ad != AddDigest::sha1 {
                if let Some(sha1) = stored_sha1 {
                    println!("SHA1: {}", sha1);
                }
            }
        }
    }

    match result {
        Some(true) => {
            println!("Hash verification: SUCCESS");
            ExitCode::SUCCESS
        },
        Some(false) => {
            println!("Hash verification: FAILURE");
            ExitCode::FAILURE
        }
        None => {
            println!("No hash verification performed");
            ExitCode::SUCCESS
        }
    }
}

fn dump(
    f: &str,
    add_digest: &Option<AddDigest>,
    stored_md5: &mut Option<String>,
    stored_sha1: &mut Option<String>,
    ignore_checksums: bool,
) -> Result<HashMap<String /*AddDigest*/, String /*hash*/>, E01Error> {
    use md5::Md5;
    use sha1::Sha1;
    use sha2::{Sha256, Sha512};

    let e01_reader = E01Reader::open_glob(f, ignore_checksums).unwrap();
    let mut hasher = HashMap::<&str, Box<dyn DynDigest>>::new();

    // record stored md5
    if let Some(md5) = e01_reader.get_stored_md5() {
        hasher.insert((&AddDigest::md5).into(), Box::new(Md5::new()));
        *stored_md5 = Some(
            md5.iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(""),
        );
    }
    // record stored sha1
    if let Some(sha1) = e01_reader.get_stored_sha1() {
        hasher.insert((&AddDigest::sha1).into(), Box::new(Sha1::new()));
        *stored_sha1 = Some(
            sha1.iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(""),
        );
    }

    if let Some(d) = &add_digest {
        match d {
            AddDigest::md5 => hasher.insert((&AddDigest::md5).into(), Box::new(Md5::new())),
            AddDigest::sha1 => hasher.insert((&AddDigest::sha1).into(), Box::new(Sha1::new())),
            AddDigest::sha256 => {
                hasher.insert((&AddDigest::sha256).into(), Box::new(Sha256::new()))
            }
            AddDigest::sha512 => {
                hasher.insert((&AddDigest::sha512).into(), Box::new(Sha512::new()))
            }
        };
    }
    let mut buf: Vec<u8> = vec![0; 1048576];
    let mut offset = 0;
    while offset < e01_reader.total_size() {
        let len = buf.len();
        let read = match e01_reader.read_at_offset(offset, &mut buf[..len]) {
            Ok(v) => v,
            Err(e) => {
                panic!("{:?}", e);
            }
        };

        if read == 0 {
            break;
        }

        hasher.iter_mut().for_each(|d| d.1.update(&buf[..read]));

        offset += read;
    }
    Ok(hasher
        .iter_mut()
        .map(|d| {
            let mut result = vec![0; d.1.output_size()];
            d.1.finalize_into_reset(&mut result).unwrap();
            (
                d.0.to_string(),
                result
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(""),
            )
        })
        .collect())
}
