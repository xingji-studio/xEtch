use super::{conflict, file_hash, invalid, plain_file, Fixture};
use crate::Result;
use std::{
    ffi::OsStr,
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE},
    Storage::{
        FileSystem::{
            FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        },
        Vhd::*,
    },
    System::{Ioctl::*, IO::DeviceIoControl},
};
pub(super) const IMAGE_NAME: &str = "fixture.vhd";
fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub(super) fn reparse(m: &Metadata) -> bool {
    m.file_attributes() & 0x400 != 0
}
pub(super) fn file_identity(f: &File) -> Result<String> {
    // SAFETY: f owns a live handle; the fixed output structure is correctly sized.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if info.nNumberOfLinks != 1 || info.dwFileAttributes & 0x410 != 0 {
        return Err(invalid(
            "fixture file is linked, a directory or a reparse point",
        ));
    }
    Ok(format!(
        "{}:{}:{}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}
pub(super) fn create_image(source: &Path, image: &Path, size: u64) -> Result<()> {
    if image.try_exists()? {
        return Err(invalid("fixture VHD must be new"));
    }
    crate::tools::command(&[
        "create".into(),
        "-f".into(),
        "vpc".into(),
        "-o".into(),
        "subformat=fixed,force_size=on".into(),
        image.as_os_str().into(),
        size.to_string().into(),
    ])?;
    let mut source = plain_file(source)?;
    let mut destination = OpenOptions::new().read(true).write(true).open(image)?;
    if destination.metadata()?.len() != size + 512 {
        return Err(invalid("unexpected fixed VHD size"));
    }
    file_identity(&destination)?;
    std::io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    // QEMU may mark fixed VHD files sparse; materialize only this newly owned file.
    let mut returned = 0;
    #[repr(C)]
    struct Sparse {
        set_sparse: u8,
    }
    let input = Sparse { set_sparse: 0 };
    if unsafe {
        DeviceIoControl(
            destination.as_raw_handle(),
            FSCTL_SET_SPARSE,
            &input as *const _ as *const _,
            size_of::<Sparse>() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    destination.sync_all()?;
    Ok(())
}
fn control<T: Default>(file: &File, code: u32) -> Result<T> {
    let mut output = T::default();
    let mut returned = 0;
    // SAFETY: synchronous call, no input, fixed-size zeroed ABI output.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            ptr::null(),
            0,
            &mut output as *mut T as *mut _,
            size_of::<T>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if returned < (size_of::<T>() as u32) {
        return Err(conflict("short native storage descriptor"));
    }
    Ok(output)
}
fn empty_control(file: &File, code: u32) -> Result<()> {
    let mut returned = 0;
    // SAFETY: synchronous control takes no input or output buffers.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            ptr::null(),
            0,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn virtual_path(handle: HANDLE) -> Result<String> {
    let mut buffer = vec![0u16; 1024];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: the VHD API receives an allocated UTF-16 output buffer and its byte size.
    let error = unsafe { GetVirtualDiskPhysicalPath(handle, &mut bytes, buffer.as_mut_ptr()) };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    Ok(String::from_utf16_lossy(
        &buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())],
    ))
}
fn open_virtual(path: &Path) -> Result<OwnedHandle> {
    let storage = VIRTUAL_STORAGE_TYPE {
        DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHD,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    };
    let mut parameters = OPEN_VIRTUAL_DISK_PARAMETERS {
        Version: OPEN_VIRTUAL_DISK_VERSION_1,
        ..Default::default()
    };
    parameters.Anonymous.Version1 = OPEN_VIRTUAL_DISK_PARAMETERS_0_0 { RWDepth: 1 };
    let mut handle = ptr::null_mut();
    // SAFETY: read-only metadata handle for this exact owned regular VHD; no attach/write API.
    let error = unsafe {
        OpenVirtualDisk(
            &storage,
            wide(path).as_ptr(),
            VIRTUAL_DISK_ACCESS_GET_INFO,
            OPEN_VIRTUAL_DISK_FLAG_NONE,
            &parameters,
            &mut handle,
        )
    };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
fn metadata_volume(path: &str) -> Result<File> {
    Ok(OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path.trim_end_matches('\\'))?)
}
fn find_volume(number: u32, f: &Fixture) -> Result<String> {
    let mut buffer = vec![0u16; 1024];
    // SAFETY: valid mutable UTF-16 buffer, search handle released on every path.
    let search = unsafe { FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if search == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    struct Search(HANDLE);
    impl Drop for Search {
        fn drop(&mut self) {
            unsafe { FindVolumeClose(self.0) };
        }
    }
    let _search = Search(search);
    let mut count = 0;
    let mut found = None;
    loop {
        count += 1;
        if count > 4096 {
            return Err(conflict("volume enumeration limit exceeded"));
        }
        let path = String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())],
        );
        if let Ok(volume) = metadata_volume(&path) {
            if let Ok(extents) =
                control::<VOLUME_DISK_EXTENTS>(&volume, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS)
            {
                let e = extents.Extents[0];
                if extents.NumberOfDiskExtents == 1
                    && e.DiskNumber == number
                    && e.StartingOffset == f.partition_offset as i64
                    && e.ExtentLength == f.partition_size as i64
                {
                    if found.is_some() {
                        return Err(conflict("ambiguous VHD volume association"));
                    }
                    found = Some(path);
                }
            }
        }
        if unsafe { FindNextVolumeW(search, buffer.as_mut_ptr(), buffer.len() as u32) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error.into());
            }
            break;
        }
    }
    found.ok_or_else(|| conflict("the owned VHD has no exact single-extent test volume"))
}
fn offline(file: &File) -> Result<()> {
    let input = SET_DISK_ATTRIBUTES {
        Version: size_of::<SET_DISK_ATTRIBUTES>() as u32,
        Persist: false,
        Attributes: DISK_ATTRIBUTE_OFFLINE,
        AttributesMask: DISK_ATTRIBUTE_OFFLINE,
        ..Default::default()
    };
    let mut returned = 0;
    // SAFETY: fixed-size native disk-attribute input for this attested owned VHD.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            IOCTL_DISK_SET_DISK_ATTRIBUTES,
            &input as *const _ as *const _,
            size_of::<SET_DISK_ATTRIBUTES>() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
struct VolumeLock(File);
impl Drop for VolumeLock {
    fn drop(&mut self) {
        let _ = empty_control(&self.0, FSCTL_UNLOCK_VOLUME);
    }
}
pub(super) struct Lease {
    file: File,
    virtual_disk: OwnedHandle,
    path: String,
    number: u32,
    base: u64,
    length: u64,
}
impl Lease {
    pub(super) fn open(root: &Path, f: &Fixture, device: Option<&Path>) -> Result<Self> {
        if device.is_some() {
            return Err(invalid(
                "Windows D1 derives the device from its owned VHD; caller paths are forbidden",
            ));
        }
        let virtual_disk = open_virtual(&root.join(&f.image_name))?;
        let path = virtual_path(virtual_disk.as_raw_handle())?;
        if !path.starts_with("\\\\.\\PhysicalDrive") {
            return Err(conflict("unexpected attached VHD path"));
        }
        let mut control_file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)?;
        let number =
            control::<STORAGE_DEVICE_NUMBER>(&control_file, IOCTL_STORAGE_GET_DEVICE_NUMBER)?
                .DeviceNumber;
        if file_hash(&mut control_file, 0, f.partition_offset)? != f.prefix_sha256 {
            return Err(conflict(
                "owned VHD protected prefix changed before quiescing",
            ));
        }
        let attributes =
            control::<GET_DISK_ATTRIBUTES>(&control_file, IOCTL_DISK_GET_DISK_ATTRIBUTES)?
                .Attributes;
        if attributes & DISK_ATTRIBUTE_READ_ONLY != 0 {
            return Err(conflict("test VHD is read-only"));
        }
        if attributes & DISK_ATTRIBUTE_OFFLINE == 0 {
            let volume = find_volume(number, f)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(volume.trim_end_matches('\\'))?;
            empty_control(&file, FSCTL_LOCK_VOLUME)?;
            let lock = VolumeLock(file);
            empty_control(&lock.0, FSCTL_DISMOUNT_VOLUME)?;
            offline(&control_file)?;
            drop(lock);
        }
        drop(control_file);
        // Remain offline across helper crashes; never remount a partially updated FAT volume.
        // The controller brings only this attested disposable VHD online after a safe terminal state.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)?;
        let mut lease = Self {
            file,
            virtual_disk,
            path,
            number,
            base: f.partition_offset,
            length: f.partition_size,
        };
        lease.verify(root, f)?;
        Ok(lease)
    }
    pub(super) fn verify(&mut self, root: &Path, f: &Fixture) -> Result<()> {
        if virtual_path(self.virtual_disk.as_raw_handle())? != self.path
            || file_identity(&plain_file(&root.join(&f.image_name))?)? != f.image_identity
        {
            return Err(conflict("VHD association changed"));
        }
        let number = control::<STORAGE_DEVICE_NUMBER>(&self.file, IOCTL_STORAGE_GET_DEVICE_NUMBER)?
            .DeviceNumber;
        let attributes =
            control::<GET_DISK_ATTRIBUTES>(&self.file, IOCTL_DISK_GET_DISK_ATTRIBUTES)?.Attributes;
        if number != self.number
            || attributes & DISK_ATTRIBUTE_OFFLINE == 0
            || attributes & DISK_ATTRIBUTE_READ_ONLY != 0
        {
            return Err(conflict("VHD identity or offline lease state changed"));
        }
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageAccessAlignmentProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut alignment = STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR::default();
        let mut returned = 0;
        // SAFETY: fixed-size native property request and descriptor buffers.
        if unsafe {
            DeviceIoControl(
                self.file.as_raw_handle(),
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const _,
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                &mut alignment as *mut _ as *mut _,
                size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>() as u32,
                &mut returned,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let physical = alignment.BytesPerPhysicalSector;
        if returned < size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>() as u32
            || alignment.BytesPerLogicalSector != 512
            || !(512..=4096).contains(&physical)
            || !physical.is_power_of_two()
            || alignment.BytesOffsetForSectorAlignment != 0
            || !f.partition_offset.is_multiple_of(u64::from(physical))
            || !f.partition_size.is_multiple_of(u64::from(physical))
        {
            return Err(conflict("unsupported VHD physical-sector geometry"));
        }
        let g = control::<DISK_GEOMETRY>(&self.file, IOCTL_DISK_GET_DRIVE_GEOMETRY)?;
        if g.BytesPerSector != 512 {
            return Err(conflict("VHD logical sector geometry changed"));
        }
        let actual_prefix = file_hash(&mut self.file, 0, f.partition_offset)?;
        if actual_prefix != f.prefix_sha256 {
            return Err(conflict(&format!(
                "VHD protected prefix changed: expected {}, actual {}",
                f.prefix_sha256, actual_prefix
            )));
        }
        Ok(())
    }
    fn range(&self, offset: u64, len: usize) -> Result<u64> {
        if !offset.is_multiple_of(512)
            || !len.is_multiple_of(512)
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > self.length)
        {
            return Err(invalid("I/O escapes the bounded test partition"));
        }
        self.base
            .checked_add(offset)
            .ok_or_else(|| invalid("I/O offset overflow"))
    }
    pub(super) fn read_at(&mut self, offset: u64, data: &mut [u8]) -> Result<()> {
        let position = self.range(offset, data.len())?;
        let mut storage = vec![0u8; data.len() + 4096];
        let start = storage.as_ptr().align_offset(4096);
        let aligned = &mut storage[start..start + data.len()];
        self.file.seek(SeekFrom::Start(position))?;
        self.file.read_exact(aligned)?;
        data.copy_from_slice(aligned);
        Ok(())
    }
    pub(super) fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let position = self.range(offset, data.len())?;
        let mut storage = vec![0u8; data.len() + 4096];
        let start = storage.as_ptr().align_offset(4096);
        let aligned = &mut storage[start..start + data.len()];
        aligned.copy_from_slice(data);
        self.file.seek(SeekFrom::Start(position))?;
        self.file.write_all(aligned)?;
        Ok(())
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
    pub(super) fn partition_hash(&mut self, len: u64) -> Result<String> {
        if len != self.length {
            return Err(invalid("hash escapes partition"));
        }
        file_hash(&mut self.file, self.base, len)
    }
}
