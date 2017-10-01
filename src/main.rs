/// This is a tiny project to be a quick alternative to symchk for generating
/// manifests. This mimics symchk of the form `symchk /om manifest /r <path>`
/// but only looks for MZ/PE files.
///
/// Due to symchk doing some weird things it can often crash or get stuck in
/// infinite loops. Thus this is a stricter (and much faster) alternative.
///
/// The output manifest is compatible with symchk and thus symchk is currently
/// used for the actual download. To download symbols after this manifest
/// has been generated use `symchk /im manifest /s <symbol path>`

use std::io;
use std::fs::File;
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Set this to true to enable status/progress messages
const STATUS_MESSAGES: bool = true;

/// Given a `path`, return a vector of all the files recursively found from
/// that path.
///
/// This eats read_dir() errors to avoid Permission Denied stuff. It could be
/// improved by being more selective with ignoring errors.
fn recursive_listdir(path: &Path) -> io::Result<Vec<PathBuf>>
{
    let mut result = Vec::new();

    if let Ok(dirlisting) = path.read_dir() {
        for entry in dirlisting {
            let path = entry?.path();

            if path.is_dir() {
                result.append(&mut recursive_listdir(&path)?);
            } else {
                result.push(path);
            }
        }
    }

    Ok(result)
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MZHeader {
    signature:       [u8; 2],
    last_page_bytes: u16,
    num_pages:       u16,
    num_relocations: u16,
    header_size:     u16,
    min_memory:      u16,
    max_memory:      u16,
    initial_ss:      u16,
    initial_sp:      u16,
    checksum:        u16,
    entry:           u32,
    ptr_relocation:  u16,
    overlay:         u16,
    reserved:        [u8; 32],
    new_header:      u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct PEHeader {
    signature:            [u8; 4],
    machine:              u16,
    num_sections:         u16,
    timestamp:            u32,
    ptr_symtable:         u32,
    num_smtable:          u32,
    optional_header_size: u16,
    characteristics:      u16,
}

const IMAGE_FILE_MACHINE_I386:  u16 = 0x014c;
const IMAGE_FILE_MACHINE_IA64:  u16 = 0x0200;
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct WindowsPEHeader32 {
    magic:                      u16,
    linker_major_version:       u8,
    linker_minor_version:       u8,
    size_of_code:               u32,
    size_of_initialized_data:   u32,
    size_of_uninitialized_data: u32,
    entry:                      u32,
    code_base:                  u32,
    data_base:                  u32,
    image_base:                 u32,
    section_align:              u32,
    file_align:                 u32,
    major_os_version:           u16,
    minor_os_version:           u16,
    major_image_version:        u16,
    minor_image_version:        u16,
    major_subsystem_version:    u16,
    minor_subsystem_version:    u16,
    win32_version:              u32,
    size_of_image:              u32,
    size_of_headers:            u32,
    checksum:                   u32,
    subsystem:                  u16,
    dll_characteristics:        u16,
    size_of_stack_reserve:      u32,
    size_of_stack_commit:       u32,
    size_of_heap_reserve:       u32,
    size_of_heap_commit:        u32,
    loader_flags:               u32,
    num_tables:                 u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct WindowsPEHeader64 {
    magic:                      u16,
    linker_major_version:       u8,
    linker_minor_version:       u8,
    size_of_code:               u32,
    size_of_initialized_data:   u32,
    size_of_uninitialized_data: u32,
    entry:                      u32,
    code_base:                  u32,
    image_base:                 u64,
    section_align:              u32,
    file_align:                 u32,
    major_os_version:           u16,
    minor_os_version:           u16,
    major_image_version:        u16,
    minor_image_version:        u16,
    major_subsystem_version:    u16,
    minor_subsystem_version:    u16,
    win32_version:              u32,
    size_of_image:              u32,
    size_of_headers:            u32,
    checksum:                   u32,
    subsystem:                  u16,
    dll_characteristics:        u16,
    size_of_stack_reserve:      u64,
    size_of_stack_commit:       u64,
    size_of_heap_reserve:       u64,
    size_of_heap_commit:        u64,
    loader_flags:               u32,
    num_tables:                 u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct ImageDataDirectory {
    vaddr: u32,
    size:  u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct ImageSectionHeader {
    name:                    [u8; 8],
    vsize:                   u32,
    vaddr:                   u32,
    raw_data_size:           u32,
    pointer_to_raw_data:     u32,
    pointer_to_relocations:  u32,
    pointer_to_line_numbers: u32,
    number_of_relocations:   u16,
    number_of_line_numbers:  u16,
    characteristics:         u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct ImageDebugDirectory {
    characteristics:      u32,
    timestamp:            u32,
    major_version:        u16,
    minor_version:        u16,
    typ:                  u32,
    size_of_data:         u32,
    address_of_raw_data:  u32,
    pointer_to_raw_data:  u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct CodeviewEntry {
    signature: [u8; 4], // RSDS
    guid_a:    u32,
    guid_b:    u16,
    guid_c:    u16,
    guid_d:    [u8; 8],
    age:       u32,
}

const IMAGE_DEBUG_TYPE_CODEVIEW: u32 = 2;

/// Read a structure from a file stream, directly interpreting the raw bytes
/// of the file as T.
///
/// User must make sure the shape of the structure `T` is safe to use in this
/// way, hence being unsafe.
unsafe fn read_struct<T: Copy>(fd: &mut File) -> io::Result<T>
{
    let mut ret: T = std::mem::zeroed();
    fd.read_exact(std::slice::from_raw_parts_mut(
            &mut ret as *mut _ as *mut u8,
            std::mem::size_of_val(&ret)))?;
    Ok(ret)
}

/// Implementation mimicing #![feature(range_contains)] for those stable rust
/// users.
fn contains(range: &std::ops::Range<u32>, item: u32) -> bool
{
    (range.start <= item) && (item < range.end)
}

/// Given a `filename`, attempt to parse out any mention of a PDB file in it.
///
/// This returns success if it successfully parses the MZ, PE, finds a debug
/// header, matches RSDS signature, and contains a valid reference to a PDB.
///
/// Returns a String of the same representation you get from `symchk` when
/// outputting a manifest. "<filename>,<guid><age>,1"
fn get_pdb(filename: &Path) -> Result<String, Box<std::error::Error>>
{
    let mut fd = File::open(filename)?;

    /* Check for an MZ header */
    let mz_header: MZHeader = unsafe { read_struct(&mut fd)? };
    if &mz_header.signature != b"MZ" {
        return Err("No MZ header present".into());
    }

    /* Seek to where the PE header should be */
    if fd.seek(SeekFrom::Start(mz_header.new_header as u64))? !=
            mz_header.new_header as u64 {
        return Err("Failed to seek to PE header".into());
    }

    /* Check for a PE header */
    let pe_header: PEHeader = unsafe { read_struct(&mut fd)? };
    if &pe_header.signature != b"PE\0\0" {
        return Err("No PE header present".into());
    }

    /* Grab the number of tables from the bitness-specific table */
    let num_tables = match pe_header.machine {
        IMAGE_FILE_MACHINE_I386 => {
            let opthdr: WindowsPEHeader32 = unsafe { read_struct(&mut fd)? };
            opthdr.num_tables
        }
        IMAGE_FILE_MACHINE_IA64 | IMAGE_FILE_MACHINE_AMD64 => {
            let opthdr: WindowsPEHeader64 = unsafe { read_struct(&mut fd)? };
            opthdr.num_tables
        }
        _ => return Err("Unsupported PE machine type".into())
    };

    /* Load all the data directories into a vector */
    let mut data_dirs = Vec::new();
    for _ in 0..num_tables {
        let datadir: ImageDataDirectory = unsafe { read_struct(&mut fd)? };
        data_dirs.push(datadir);
    }

    /* Debug directory is at offset 6, validate we have at least 7 entries */
    if data_dirs.len() < 7 {
        return Err("No debug data directory".into());
    }

    /* Grab the debug table */
    let debug_table = data_dirs[6];
    if debug_table.vaddr == 0 || debug_table.size == 0 {
        return Err("Debug directory not present or zero sized".into());
    }

    /* Validate debug table size is sane */
    let iddlen = std::mem::size_of::<ImageDebugDirectory>() as u32;
    let debug_table_ents = debug_table.size / iddlen;
    if (debug_table.size % iddlen) != 0 || debug_table_ents == 0 {
        return Err("No debug entries or not mod ImageDebugDirectory".into());
    }

    /* Seek to where the section table should be */
    let section_headers = mz_header.new_header as u64 + 0x18 +
                          pe_header.optional_header_size as u64;
    if fd.seek(SeekFrom::Start(section_headers))? != section_headers {
        return Err("Failed to seek to section table".into());
    }

    /* Parse all the sections into a vector */
    let mut sections = Vec::new();
    for _ in 0..pe_header.num_sections {
        let sechdr: ImageSectionHeader = unsafe { read_struct(&mut fd)? };
        sections.push(sechdr);
    }

    /* Find the section the debug table belongs to */
    let mut debug_data = None;
    for section in &sections {
        /* We use raw_data_size instead of vsize as we are not loading the
         * file and only care about raw contents in the file.
         */
        let secrange = section.vaddr..section.vaddr + section.raw_data_size;

        /* Check if the entire debug table is contained in this sections
         * virtual address range.
         */
        if contains(&secrange, debug_table.vaddr) &&
                contains(&secrange, debug_table.vaddr + debug_table.size - 1) {
            debug_data = Some(debug_table.vaddr - section.vaddr +
                              section.pointer_to_raw_data);
            break;
        }
    }

    if debug_data.is_none() {
        return Err("Unable to find debug data".into());
    }
    let debug_raw_ptr = debug_data.unwrap() as u64;

    /* Seek to where the debug directories should be */
    if fd.seek(SeekFrom::Start(debug_raw_ptr))? != debug_raw_ptr {
        return Err("Failed to seek to debug directories".into());
    }

    /* Look through all debug table entries for codeview entries */
    for _ in 0..debug_table_ents {
        let de: ImageDebugDirectory = unsafe { read_struct(&mut fd)? };

        if de.typ == IMAGE_DEBUG_TYPE_CODEVIEW {
            /* Seek to where the codeview entry should be */
            let cvo = de.pointer_to_raw_data as u64;
            if fd.seek(SeekFrom::Start(cvo))? != cvo {
                return Err("Failed to seek to codeview entry".into());
            }

            let cv: CodeviewEntry = unsafe { read_struct(&mut fd)? };
            if &cv.signature != b"RSDS" {
                return Err("No RSDS signature present in codeview ent".into());
            }

            /* Calculate theoretical string length based on the size of the
             * section vs the size of the header */
            let cv_strlen = de.size_of_data as usize -
                std::mem::size_of_val(&cv);

            /* Read in the debug path */
            let mut dpath = vec![0u8; cv_strlen];
            fd.read_exact(&mut dpath)?;

            /* PDB strings are utf8 and null terminated, find the first null
             * and we will split it there.
             */
            if let Some(null_strlen) = dpath.iter().position(|&x| x == 0) {
                let dpath = std::str::from_utf8(&dpath[..null_strlen])?;

                /* Further, since this path can be a full path, we get only
                 * the filename component of this path.
                 */
                if let Some(filename) = Path::new(dpath).file_name() {
                    /* This is the format string used by symchk.
                     * Original is in SymChkCheckFiles()
                     * "%s,%08X%04X%04X%02X%02X%02X%02X%02X%02X%02X%02X%x,1"
                     */
                    let guidstr = format!("{},{:08X}{:04X}{:04X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:x},1",
                                          filename.to_str().unwrap(),
                                          cv.guid_a, cv.guid_b, cv.guid_c,
                                          cv.guid_d[0], cv.guid_d[1],
                                          cv.guid_d[2], cv.guid_d[3],
                                          cv.guid_d[4], cv.guid_d[5],
                                          cv.guid_d[6], cv.guid_d[7],
                                          cv.age);
                    return Ok(guidstr)
                } else {
                    return Err("Could not parse file from RSDS path".into())
                }
            } else {
                return Err("Failed to find null terminiator in RSDS".into())
            }
        }
    }

    Err("Failed to find RSDS codeview directory".into())
}

fn main()
{
    /* List all files in system32 */
    print!("Generating file listing...\n");
    let listing = recursive_listdir(&Path::new("C:\\windows\\system32")).
        expect("Failed to list directory");
    print!("Done!\n");

    /* For each file, try to parse PDB information out of it and print the
     * manifest-style information to the screen.
     */
    let mut output_pdbs = Vec::new();
    for (ii, filename) in listing.iter().enumerate() {
        if STATUS_MESSAGES {
            print!("\rParsed {} of {} files ({} pdbs)",
                ii, listing.len(), output_pdbs.len());
        }
        if let Ok(manifest_str) = get_pdb(&filename) {
            output_pdbs.push(manifest_str);
        }
    }

    let mut output_file = File::create("manifest").
        expect("Failed to create output manifest file");
    output_file.write_all(output_pdbs.join("\n").as_bytes()).
        expect("Failed to write to manifest file");
}
