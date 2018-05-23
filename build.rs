extern crate glob;
extern crate serde_json;

use glob::glob;
use std::fs::File;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    for entry in glob("truffle/contracts/*.sol").unwrap() {
        match entry {
            Ok(path) => println!("cargo:rerun-if-changed={}", path.display()),
            Err(e) => eprintln!("error finding contracts: {:?}", e),
        }
    }

    match Command::new("truffle")
        .current_dir("truffle")
        .arg("compile")
        .status()
    {
        Ok(status) => {
            if !status.success() {
                if let Some(code) = status.code() {
                    panic!("`truffle compile` exited with error exit code {}", code);
                } else {
                    panic!("`truffle compile` was terminated by a signal");
                }
            }
        }
        Err(e) => {
            if let std::io::ErrorKind::NotFound = e.kind() {
                panic!("`truffle` executable not found in $PATH, please install with `npm i -g truffle`");
            } else {
                panic!("an error occurred when running `truffle compile`: {}", e);
            }
        }
    }

    std::fs::create_dir("abi").ok();

    for entry in glob("truffle/build/contracts/*.json").unwrap() {
        match entry {
            Ok(path) => {
                let file = File::open(&path)
                    .expect(&format!("unable to open compiled json file: {:?}", &path));
                let json: serde_json::Value = serde_json::from_reader(file)
                    .expect(&format!("unable to parse compiled json file: {:?}", &path));

                let mut out_path = PathBuf::from("./abi");
                out_path.push(&path.file_name().expect("no file name component to path"));
                out_path.set_extension("abi");

                let mut out_file = File::create(&out_path).expect(&format!(
                    "could not create output abi file: {:?}",
                    &out_path
                ));

                serde_json::to_writer_pretty(out_file, &json["abi"]).expect(&format!(
                    "could not write abi to output file: {:?}",
                    &out_path
                ));
            }
            Err(e) => eprintln!("{:?}", e),
        }
    }
}
