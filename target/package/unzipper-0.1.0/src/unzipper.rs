//! Unzipper Module.
//!
//! This module provides functionality to unzip files from a zip archive.
//! It reads the central directory, extracts file entries, and allows access to the files within the archive.
//! It supports both compressed and uncompressed files, and handles errors related to zip operations.
//! It can be used to read files from zip archives, such as EPUB files, and extract their contents.
//! It is designed to be efficient and easy to use, providing methods to open zip files, read file entries, and extract files into memory.
#![allow(dead_code)]

use log::debug;

use core::fmt;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem::size_of;
use std::path::{Path, PathBuf};

use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZFlush};

// File header:

// central file header signature   4 bytes  (0x02014b50)
// version made by                 2 bytes
// version needed to extract       2 bytes
// general purpose bit flag        2 bytes
// compression method              2 bytes
// last mod file time              2 bytes
// last mod file date              2 bytes
// crc-32                          4 bytes
// compressed size                 4 bytes
// uncompressed size               4 bytes
// file name length                2 bytes
// extra field length              2 bytes
// file comment length             2 bytes
// disk number start               2 bytes
// internal file attributes        2 bytes
// external file attributes        4 bytes
// relative offset of local header 4 bytes

// file name (variable size)
// extra field (variable size)
// file comment (variable size)
#[repr(packed(1))]
struct DirFileHeader {
    signature: u32,
    version: u16,
    extract_version: u16,
    flags: u16,
    compresion_method: u16,
    last_mod_time: u16,
    last_mod_date: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    file_path_length: u16,
    extra_field_length: u16,
    comment_field_length: u16,
    disk_number_start: u16,
    internal_file_attr: u16,
    external_file_attr: u32,
    header_offset: u32,
}

// Local header record.

// local file header signature     4 bytes  (0x04034b50)
// version needed to extract       2 bytes
// general purpose bit flag        2 bytes
// compression method              2 bytes
// last mod file time              2 bytes
// last mod file date              2 bytes
// crc-32                          4 bytes
// compressed size                 4 bytes
// uncompressed size               4 bytes
// file name length                2 bytes
// extra field length              2 bytes

// file name (variable size)
// extra field (variable size)
#[repr(packed(1))]
#[derive(Debug, Clone, Copy)]
struct FileHeader {
    signature: u32,
    extract_version: u16,
    flags: u16,
    compression_method: u16,
    last_mod_time: u16,
    last_mod_date: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    file_path_length: u16,
    extra_field_length: u16,
}

const DIR_FILE_HEADER_SIGNATURE: u32 = 0x02014b50;
const FILE_HEADER_SIGNATURE: u32 = 0x04034b50;
const DIR_END_SIGNATURE: u32 = 0x06054b50;

const BUFFER_SIZE: usize = 1024 * 16;
const FILE_CENTRAL_SIZE: usize = 22;

#[derive(Debug, Default, Clone)]
struct FileEntry {
    start_pos: u32,       // in zip file
    compressed_size: u32, // in zip file
    size: u32,            // once decompressed
    method: u16,          // compress method (0 = not compressed, 8 = DEFLATE)
}

type FileEntries = HashMap<String, FileEntry>;

/// Struct that provides functionality to unzip files from a zip archive.
///
/// It reads the central directory, extracts file entries, and allows access to the files within the archive.
/// It supports both compressed and uncompressed files, and handles errors related to zip operations.
/// It can be used to read files from zip archives, such as EPUB files, and extract their contents.
/// It is designed to be efficient and easy to use, providing methods to open zip files, read file entries, and extract files into memory.
pub struct Unzipper {
    filepath: PathBuf, // The path to the zip file
    file: Option<File>,
    file_entries: FileEntries,
    current_file_entry: Option<FileEntry>,
    current_file_header: Option<FileHeader>,
}

/// Implements the Debug trait for Unzipper to provide a formatted output of its state.
impl fmt::Debug for Unzipper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // As HashMap is not ordered, we need to sort the entries for comparison in testings
        let mut v: Vec<_> = self.file_entries.iter().collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));

        f.debug_struct("Specificity")
            .field(
                "file name",
                &self.filepath.file_name().unwrap_or("not found".as_ref()),
            )
            .field("file_entries", &v)
            .field("current_file_entry", &self.current_file_entry)
            .field("current_file_header", &self.current_file_header)
            .finish()
    }
}

impl Unzipper {
    /// Creates a new Unzipper instance for the specified file path.
    ///
    /// # Arguments
    /// * `filepath` - A reference to the path of the zip file to be opened.
    ///
    /// # Returns
    /// A Result containing the Unzipper instance if successful, or an error if the file could not be opened.
    pub fn new(filepath: &Path) -> Result<Unzipper, std::io::Error> {
        let mut unzipper = Unzipper {
            filepath: filepath.to_path_buf(),
            file: None,
            file_entries: FileEntries::new(),
            current_file_entry: None,
            current_file_header: None,
        };
        unzipper.open(filepath)?;
        Ok(unzipper)
    }

    /// Returns the u32 value from the given byte slice.
    ///
    /// # Arguments
    /// * `bytes` - A byte slice containing the data to be converted to u32.
    ///
    /// # Returns
    /// The u32 value extracted from the byte slice, or a default value of 0 if the slice is not exactly 4 bytes long.
    #[inline]
    fn get_u32(&self, bytes: &[u8]) -> u32 {
        let bytes: &[u8; 4] = bytes.try_into().unwrap_or(&[0; 4]);
        u32::from_le_bytes(*bytes)
    }

    /// Returns the u16 value from the given byte slice.
    ///
    /// # Arguments
    /// * `bytes` - A byte slice containing the data to be converted to u16.
    ///
    /// # Returns
    /// The u16 value extracted from the byte slice, or a default value of 0 if the slice is not exactly 2 bytes long.
    #[inline]
    fn get_u16(&self, bytes: &[u8]) -> u16 {
        let bb: &[u8; 2] = bytes.try_into().unwrap_or(&[0; 2]);
        return u16::from_le_bytes(*bb);
    }

    /// Cleans the file path by removing unnecessary parts like empty segments, current directory indicators (.), and parent directory indicators (..).
    ///
    /// # Arguments
    /// * `path` - A string slice representing the file path to be cleaned.
    ///
    /// # Returns
    /// A cleaned string representing the file path, with unnecessary segments removed.
    pub fn clean_file_path(&self, path: &str) -> String {
        let mut parts = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => continue, // skip empty or current dir
                ".." => {
                    parts.pop();
                } // go up one directory
                _ => parts.push(part),
            }
        }
        let cleaned = parts.join("/");
        if path.starts_with('/') {
            format!("/{}", cleaned)
        } else {
            cleaned
        }
    }

    /// Reads data from the zip file at the specified position into the provided buffer.
    ///
    /// # Arguments
    /// * `buffer` - A mutable byte slice where the data will be read into.
    /// * `position` - The position in the file to start reading from.
    /// * `msg` - A string slice representing the message to be used in case of an error.
    ///
    /// # Returns
    /// A Result indicating success or an error if the read operation fails.
    fn get_data(
        &mut self,
        buffer: &mut [u8],
        position: usize,
        msg: &str,
    ) -> Result<(), std::io::Error> {
        if let Some(ref mut file) = self.file {
            if file.seek(SeekFrom::Start(position as u64))? != position as u64 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unable to seek to {msg}"),
                ));
            }
            let length = buffer.len();
            file.read_exact(&mut buffer[..length])
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "File not open",
            ));
        }
    }

    /// Opens a zip file at the specified path and reads its central directory.
    ///
    /// # Arguments
    /// * `path` - A reference to the path of the zip file to be opened.
    ///
    /// # Returns
    /// A Result indicating success or an error if the file could not be opened or if the zip file is invalid.
    pub fn open(&mut self, path: &Path) -> Result<(), std::io::Error> {
        self.file = Some(File::open(path.canonicalize()?)?);

        if let Some(ref mut file) = self.file {
            // Seek to beginning of central directory
            //
            // We seek the file back until we reach the "End Of Central Directory"
            // signature "PK\5\6". (ecd_offset)
            //
            // end of central dir signature    4 bytes  (0x06054b50)
            // number of this disk             2 bytes   4
            // number of the disk with the
            // start of the central directory  2 bytes   6
            // total number of entries in the
            // central directory on this disk  2 bytes   8
            // total number of entries in
            // the central directory           2 bytes  10
            // size of the central directory   4 bytes  12
            // offset of start of central
            // directory with respect to
            // the starting disk number        4 bytes  16
            // .ZIP file comment length        2 bytes  20
            // --- SIZE UNTIL HERE: UNZIP_EOCD_SIZE ---
            // .ZIP file comment       (variable size)

            let length = file.seek(SeekFrom::End(0))? as usize;

            // Get the length of the file in bytes and check if it is large enough
            // to be a valid zip file
            if length < FILE_CENTRAL_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "File is too small to be a zip file",
                ));
            }
            let mut ecd_offset = length - FILE_CENTRAL_SIZE;

            let mut buff = [0; FILE_CENTRAL_SIZE + 5];
            self.get_data(
                &mut buff[..FILE_CENTRAL_SIZE],
                ecd_offset,
                "end of central directory",
            )?;

            // Check if the end of central directory signature is present

            if self.get_u32(&buff[..4]) != DIR_END_SIGNATURE {
                // There must be a comment in the last entry. Search for the beginning of that entry
                // going backwards in the file.
                // We will search backwards in 64kB blocks until we find the signature
                // "PK\5\6" or we reach the beginning of the file.

                let end_offset = if ecd_offset > 65536 {
                    ecd_offset - 65536
                } else {
                    0
                };

                ecd_offset = if ecd_offset >= FILE_CENTRAL_SIZE {
                    ecd_offset - FILE_CENTRAL_SIZE
                } else {
                    0
                };

                let mut found = false;
                while !found && (ecd_offset > end_offset) {
                    self.get_data(&mut buff, ecd_offset, "end of central directory")?;

                    // Check if the end of central directory signature is present
                    if self.get_u32(&buff[0..4]) == DIR_END_SIGNATURE {
                        found = true;
                        break;
                    }

                    if let Some(p) = buff.windows(4).position(|w| w == b"PK\x05\x06") {
                        ecd_offset += p;
                        self.get_data(
                            &mut buff[..FILE_CENTRAL_SIZE],
                            ecd_offset,
                            "end of central directory",
                        )?;

                        found = true;
                        break;
                    }
                    ecd_offset -= FILE_CENTRAL_SIZE;
                }

                if !found {
                    ecd_offset = 0;
                }
            }

            if (ecd_offset == 0) || (self.get_u32(&buff[0..4]) != DIR_END_SIGNATURE) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Unable to find end of central directory",
                ));
            }

            let start_offset = self.get_u32(&buff[16..20]) as usize;
            let mut num_entries = self.get_u16(&buff[10..12]);
            let entries_total_size = ecd_offset - start_offset;
            let mut entries = vec![0; entries_total_size];

            self.get_data(&mut entries, start_offset, "central directory")?;

            // Check if the central directory signature is present
            if self.get_u32(&entries[0..4]) != DIR_FILE_HEADER_SIGNATURE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Unable to find central directory",
                ));
            }

            let mut file_entry_offset: usize = 0;

            while num_entries > 0 {
                let dir_file_header =
                    unsafe { &*(entries.as_ptr().add(file_entry_offset) as *const DirFileHeader) };

                if dir_file_header.signature != DIR_FILE_HEADER_SIGNATURE {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Invalid central directory signature",
                    ));
                }

                let f_name = unsafe {
                    let start = file_entry_offset + std::mem::size_of::<DirFileHeader>();
                    let end = start + dir_file_header.file_path_length as usize;
                    std::str::from_utf8_unchecked(&entries[start..end])
                };
                let file_path = self.clean_file_path(f_name);

                let file_entry = FileEntry {
                    start_pos: dir_file_header.header_offset,
                    compressed_size: dir_file_header.compressed_size,
                    size: dir_file_header.uncompressed_size,
                    method: dir_file_header.compresion_method,
                };

                self.file_entries.insert(file_path, file_entry);

                file_entry_offset += std::mem::size_of::<DirFileHeader>()
                    + dir_file_header.file_path_length as usize
                    + dir_file_header.extra_field_length as usize
                    + dir_file_header.comment_field_length as usize;

                num_entries -= 1;
            }
            Ok(())
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Zip file not open",
            ));
        }
    }

    /// Returns the size of the currently opened file entry.
    ///
    /// # Returns
    /// A Result containing the size of the file entry in bytes if successful, or an error message if no file is open.
    fn get_file_size(&self) -> Result<u32, String> {
        match self.current_file_entry {
            Some(ref entry) => Ok(entry.size),
            None => Err("File not open".to_string()),
        }
    }

    /// Checks if a file exists in the zip archive.
    ///
    /// # Arguments
    /// * `file_path` - A string slice representing the path of the file to check.
    ///
    /// # Returns
    /// A boolean indicating whether the file exists in the archive.
    fn file_exists(&self, file_path: &str) -> bool {
        let cleaned_file_path = self.clean_file_path(file_path);
        self.file_entries.get(&cleaned_file_path).is_some()
    }

    /// Opens a file entry in the zip archive.
    ///
    /// # Arguments
    /// * `file_path` - A string slice representing the path of the file to open.
    ///
    /// # Returns
    /// A Result indicating success or an error if the file could not be opened or if the file is not found.
    /// This method reads the file header and checks the signature and compression method.
    fn open_file(&mut self, file_path: &str) -> Result<(), std::io::Error> {
        let cleaned_file_path = self.clean_file_path(file_path);

        if let Some(file_entry) = self.file_entries.get(&cleaned_file_path) {
            self.current_file_entry = Some(file_entry.clone());

            // Extract the start position before calling get_data
            let start_pos = file_entry.start_pos as usize;

            // Use a temporary buffer to avoid borrowing self multiple times
            let mut temp_buffer = vec![0; size_of::<FileHeader>()];
            self.get_data(&mut temp_buffer, start_pos, "file header")?;
            self.current_file_header =
                unsafe { Some(*(temp_buffer.as_ptr() as *const FileHeader)) };

            if let Some(file_header) = &self.current_file_header {
                let signature = file_header.signature; // Copy to local variable
                let compression_method = file_header.compression_method; // Copy to local variable

                if signature != FILE_HEADER_SIGNATURE {
                    self.close_file();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Invalid file header signature: {}", signature),
                    ));
                }
                if compression_method != 0 && compression_method != 8 {
                    self.close_file();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Unsupported compression method: {}", compression_method),
                    ));
                }
            }

            Ok(())
        } else {
            self.close_file();
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("File not found {cleaned_file_path}"),
            ))
        }
    }

    /// Closes the currently opened file entry by resetting the current file entry and header.
    /// This method is called after the file has been read or if an error occurs.
    /// It ensures that the Unzipper is ready to open a new file entry.
    /// It does not close the underlying file, allowing for multiple file reads without reopening the zip file.
    /// It is important to call this method after finishing with a file to avoid leaving the Unzipper in an inconsistent state.
    /// It is also used internally to reset the state of the Unzipper when opening a new file.
    /// # Returns
    /// None
    fn close_file(&mut self) {
        self.current_file_entry = None;
        self.current_file_header = None;
    }

    /// Displays the file entries available in the zip archive.
    ///
    /// This method iterates over the file entries and prints their details, including:
    /// - Position in the zip file
    /// - Compressed size
    /// - Uncompressed size
    /// - Compression method
    /// - File name
    ///
    /// It is useful for debugging and understanding the contents of the zip archive.
    /// It prints a header before the list and a footer after the list to indicate the end of the entries.
    /// # Returns
    /// None
    pub fn show_file_entries(&self) {
        debug!("---- Files available: ----");
        for (name, entry) in &self.file_entries {
            debug!(
                "pos: {:<7} zip size: {:<7} out size: {:<7} method: {:<1} name: <{}>",
                entry.start_pos, entry.compressed_size, entry.size, entry.method, name
            );
        }
        debug!("[End of List]");
    }

    /// Unzips a file from the archive into a bytes vector.
    ///
    /// Returns an error if the file is not found or decompression fails.
    /// Uses an iterator with 8192 byte buffer for reading compressed data.
    pub fn get_file(&mut self, file_path: &str) -> Result<Vec<u8>, std::io::Error> {
        // Open the file entry in the zip
        self.open_file(file_path)?;

        let file_entry = match &self.current_file_entry {
            Some(entry) => entry,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "File entry not found",
                ));
            }
        };
        let file_header = match &self.current_file_header {
            Some(header) => header,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "File header not found",
                ));
            }
        };

        // Calculate the offset to the file data
        let data_offset = file_entry.start_pos as usize
            + std::mem::size_of::<FileHeader>()
            + file_header.file_path_length as usize
            + file_header.extra_field_length as usize;

        // Prepare output buffer
        let mut output = vec![0u8; file_entry.size as usize];

        match file_entry.method {
            0 => {
                // No compression, just copy
                self.get_data(output.as_mut_slice(), data_offset, "file data")?;
            }
            8 => {
                // Deflate compression using iterator with BUFFER_SIZE byte buffer
                let mut compressed_size = file_entry.compressed_size as usize;
                let mut buffer = vec![0u8; BUFFER_SIZE];

                let mut inflate_state = InflateState::new(DataFormat::Raw);
                let mut output_pos = 0;

                while compressed_size > 0 {
                    let chunk_size = std::cmp::min(BUFFER_SIZE, compressed_size);
                    let pos = data_offset + output_pos;
                    self.get_data(&mut buffer[..chunk_size], pos, "compressed data")?;

                    let stream_result = inflate(
                        &mut inflate_state,
                        &buffer[..chunk_size],
                        &mut output[output_pos..],
                        if compressed_size <= BUFFER_SIZE {
                            MZFlush::Finish
                        } else {
                            MZFlush::None
                        },
                    );
                    if stream_result.status.is_err() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Decompression failed",
                        ));
                    }
                    output_pos += stream_result.bytes_written;
                    compressed_size -= chunk_size;
                }
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Unsupported compression method",
                ));
            }
        }

        self.close_file();
        Ok(output)
    }
}

#[cfg(test)]
mod unzipper_tests {
    use super::*;

    use test_support::unit_test::UnitTest;

    #[test]
    fn test_unzipper_open_epub_file() {
        let unit_test = UnitTest::new("unzipper_open_epub_file");

        println!(
            "Unzipper Test Case Folder: {:?}",
            unit_test.test_case_folder()
        );

        let files = unit_test.get_test_case_file_paths().unwrap();

        for file in files {
            let file_name = file.file_name().unwrap().to_str().unwrap();

            if file_name.ends_with(".epub") {
                println!("Unzipper Testing File: {:?}", file_name);

                let unzipper = Unzipper::new(&file);
                assert!(
                    unzipper.is_ok(),
                    "Failed to open epub file: {:?}",
                    file_name
                );

                let data = format!("{:#?}", unzipper);
                assert!(unit_test.check_result_with_file(&data, &file_name));
            }
        }
    }
}
