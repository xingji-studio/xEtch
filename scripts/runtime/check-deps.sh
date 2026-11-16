#!/bin/sh
# No Python/build tools needed. Run from any working directory.
if [ "${XETCH_PACKAGE_CHECK:-}" = isolated ]; then
    unset XETCH_EXT4_ENGINE XETCH_EXT4_WORKER XETCH_BACKEND XETCH_NTFS_WORKER XETCH_QEMU_IMG XETCH_QEMU_STORAGE_DAEMON QEMU_MODULE_DIR LD_LIBRARY_PATH DYLD_LIBRARY_PATH
    PATH=/usr/bin:/bin
    export PATH
fi
package_dir=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1
if [ ! -f "$package_dir/runtime-files.txt" ]; then
    echo "[ERROR] Missing runtime-files.txt. Restore the complete package folder."
    exit 1
fi
while IFS= read -r file; do
    if [ ! -f "$package_dir/$file" ]; then
        echo "[ERROR] Missing packaged file: $file. Restore the complete folder."
        exit 1
    fi
done < "$package_dir/runtime-files.txt"
if [ ! -x "$package_dir/xetch" ]; then
    echo "[ERROR] xetch is missing or not executable. Keep the entire package folder together."
    exit 1
fi
if ! "$package_dir/xetch" --version; then
    echo "[ERROR] The backend cannot start. Check OS/CPU compatibility and system C runtime libraries."
    exit 1
fi
for tool in qemu-img qemu-storage-daemon; do
    if [ ! -x "$package_dir/qemu/$tool" ]; then
        echo "[ERROR] Missing bundled $tool. Restore or repackage the complete folder."
        exit 1
    fi
done
diagnostics=$("$package_dir/xetch" --json doctor) || exit 1
if printf '%s\n' "$diagnostics" | grep -Eq '"available"[[:space:]]*:[[:space:]]*false'; then
    echo "[ERROR] QEMU cannot start. Check the missing shared library/version reported below."
    printf '%s\n' "$diagnostics"
    echo "Install compatible system runtime libraries or use a package built for this OS."
    exit 1
fi
echo "[OK] Backend and bundled QEMU tools can start."
if printf '%s\n' "$diagnostics" | grep -Eq '"ntfs_write"[[:space:]]*:[[:space:]]*true'; then
    echo "[OK] NTFS write worker is ready."
elif [ -f "$package_dir/xetch-ntfs-worker" ]; then
    echo "[ERROR] The bundled NTFS worker cannot start or has an incompatible version."
    printf '%s\n' "$diagnostics"
    exit 1
else
    echo "[NOTE] Optional NTFS worker is absent; NTFS is read-only. Other supported filesystems remain available."
fi
ext4_worker=${XETCH_EXT4_WORKER:-"$package_dir/xetch-ext4-worker"}
if [ -f "$ext4_worker" ]; then
    version=$("$ext4_worker" --version) || { echo "[ERROR] BSD ext4 worker cannot start; restore the worker and its runtime libraries."; exit 1; }
    if ! printf '%s\n' "$version" | grep -q '"profile":"bsd-xattr-acl-v1"'; then
        echo "[ERROR] BSD ext4 worker has an incompatible profile. Rebuild/package the matching worker."
        exit 1
    fi
    echo "[OK] BSD ext4 xattr/ACL worker is ready. Set XETCH_EXT4_ENGINE=lwext4 to select it."
elif [ "${XETCH_EXT4_ENGINE:-fstool}" = lwext4 ]; then
    echo "[ERROR] Selected BSD ext4 worker is missing. Repackage with XETCH_EXT4=on or correct XETCH_EXT4_WORKER."
    exit 1
else
    echo "[NOTE] Optional BSD ext4 worker absent; the default fstool ext4 engine remains available."
fi
echo "[OK] Runtime dependency check passed."
