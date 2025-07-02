#![allow(non_upper_case_globals, non_snake_case, non_camel_case_types)]

use clap::Parser;
use std::{
    collections::HashSet,
    iter::FromIterator,
    ops::BitAndAssign,
    process::ExitCode
};
use tracing_subscriber::{
    filter::LevelFilter,
    layer::SubscriberExt,
    util::SubscriberInitExt
};

use e01::{
    e01_reader::{E01Error, E01Reader},
    hasher::{HashType, MultiHasher}
};

#[derive(Parser)]
#[command(author, version, about, long_about)]
struct Args {
    /// Path to input file.
    input: String,

    /// Calculate additional digest (hash) types
    #[arg(short = 'd', long = "digest", value_enum, name = "hash")]
    extra_hashes: Vec<HashType>,

    /// Ignore all checksums during read, default value is false
    #[arg(short, long, default_value = "false")]
    ignore_checksums: bool
}

struct OptBool(Option<bool>);

impl BitAndAssign for OptBool {
    fn bitand_assign(&mut self, rhs: Self) {
        match (self.0.as_mut(), rhs.0) {
            (Some(l), Some(r)) => *l &= r,
            (None, Some(_)) => *self = rhs,
            _ => {}
        };
    }
}

fn check_hash<H1: AsRef<[u8]>, H2: AsRef<[u8]>>(
    htype: HashType,
    hash_act: Option<H1>,
    hash_exp: Option<H2>
) -> OptBool
{
    OptBool(
        match hash_act {
            Some(hash_act) => match hash_exp {
                Some(hash_exp) if hash_act.as_ref() != hash_exp.as_ref() => {
                    println!(
                        "{} {} != {}",
                        htype,
                        hex::encode(hash_act),
                        hex::encode(hash_exp)
                    );
                    Some(false)
                },
                _ => {
                    println!("{} {} ok", htype, hex::encode(hash_act));
                    Some(true)
                }
            }
            None => None
        }
    )
}

fn run(args: Args)-> Result<ExitCode, E01Error> {
    let e01_reader = E01Reader::open_glob(&args.input, args.ignore_checksums)?;

    let mut htypes: HashSet<HashType> = HashSet::from_iter(args.extra_hashes);

    let stored_md5 = e01_reader.get_stored_md5();
    let stored_sha1 = e01_reader.get_stored_sha1();

    // compute MD5 if we have one stored
    if stored_md5.is_some() {
        htypes.insert(HashType::MD5);
    }

    // compute SHA1 if we have one stored
    if stored_sha1.is_some() {
        htypes.insert(HashType::SHA1);
    }

    let mut hasher = MultiHasher::from(htypes);

    // read through the image
    let mut buf: Vec<u8> = vec![0; 1048576];
    let mut offset = 0;
    while offset < e01_reader.total_size() {
        let read = e01_reader.read_at_offset(offset, &mut buf)?;
        hasher.update(&buf[..read]);
        offset += read;
    }

    let hashes = hasher.finalize();

    // verify and output hashes
    let mut checked = check_hash(
        HashType::MD5,
        hashes.get(&HashType::MD5),
        stored_md5
    );

    checked &= check_hash(
        HashType::SHA1,
        hashes.get(&HashType::SHA1),
        stored_sha1
    );

    if let Some(sha256) = hashes.get(&HashType::SHA256) {
        println!("{} {}", HashType::SHA256, hex::encode(sha256));
    }

    Ok(
        match checked.0 {
            Some(false) => {
                println!("Hash verification: FAILURE");
                ExitCode::FAILURE
            },
            None => {
                println!("No hash verification performed");
                ExitCode::SUCCESS
            },
            Some(true) => {
                println!("Hash verification: SUCCESS");
                ExitCode::SUCCESS
            }
        }
    )
}

fn main() -> ExitCode {
    let stderr_layer = tracing_subscriber::fmt::layer()
//        .with_current_span(true)
        .without_time()
        .with_file(false)
        .with_line_number(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_target(false)
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(LevelFilter::INFO)
//        .with(LevelFilter::DEBUG)
        .init();

    let args = Args::parse();

    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", e);
            ExitCode::FAILURE
        }
    }
}
