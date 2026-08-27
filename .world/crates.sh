# The crates that expose a C API and are built/installed through cargo-c.
# Sourced by build.sh and install.sh after build_config.sh.
CRATES="vmdk e01 rawdisk"

# Only vmdk builds for 32-bit Windows.
if [[ $Target == 'windows' && $Architecture == '32' ]]; then
  CRATES="vmdk"
fi
