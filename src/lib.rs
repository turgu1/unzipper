//! A library for Zip file extraction.
//!
//! This library provides functionality to unzip files that are compressed in the ZIP (PKZip) format, including reading the directory,
//! extracting files, and handling errors related to zip operations. Internally, it uses the `miniz_oxide` crate for low-level ZIP file handling.
//!
//! The unzipper is open-source and can be freely used and modified under the terms of the MIT license.

pub mod unzipper;

pub use unzipper::Unzipper;
