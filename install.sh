#!/usr/bin/env bash
# Per-user installer for Rustovan GitHub release binaries.
set -Eeuo pipefail

readonly REPOSITORY="MinitorMHS/rustovan"
readonly API_ROOT="https://api.github.com/repos/${REPOSITORY}"
readonly WRAPPER_MARKER="# managed by rustovan installer"
readonly PROFILE_START="# >>> rustovan installer >>>"
readonly PROFILE_END="# <<< rustovan installer <<<"

action="${1:-install}"
if (($# > 1)); then
  printf 'Usage: %s [install|update|uninstall|help]\n' "$0" >&2
  exit 2
fi
case "$action" in
  install|update|uninstall|help) ;;
  *) printf 'Unknown action: %s\nUsage: %s [install|update|uninstall|help]\n' "$action" "$0" >&2; exit 2 ;;
esac

if [[ "$action" == help ]]; then
  cat <<'HELP'
Rustovan per-user installer

Usage: bash install.sh [install|update|uninstall|help]

Install checks the latest stable GitHub release and installs or updates the
matching binary. Uninstall removes only files and PATH entries managed by this
installer. Run `rustovan --help` from a new terminal after installation.

The repository is private unless its owner makes it public. For a private
repository, set RUSTOVAN_GITHUB_TOKEN (or GH_TOKEN/GITHUB_TOKEN) to a token with
read access to releases. Never paste a token into this script or commit it.
HELP
  exit 0
fi

if [[ -z "${HOME:-}" ]]; then
  echo 'HOME is not set; cannot choose a per-user install location.' >&2
  exit 1
fi
data_root="$HOME/.local/share/rustovan"
app_dir="$data_root/app"
installed_binary="$app_dir/rustovan"
metadata_file="$app_dir/install.properties"
marker_file="$app_dir/.rustovan-managed"
command_dir="$HOME/.local/bin"
command_path="$command_dir/rustovan"

is_safe_file() {
  [[ -e "$1" && ! -L "$1" && -f "$1" ]]
}

copy_file_mode() {
  local source="$1" destination="$2" mode=''
  mode="$(stat -c '%a' "$source" 2>/dev/null || stat -f '%Lp' "$source" 2>/dev/null || true)"
  if [[ "$mode" =~ ^[0-7]{3,4}$ ]]; then chmod "$mode" "$destination"; fi
}

remove_profile_block() {
  local file="$1" temp
  [[ -e "$file" ]] || return 0
  if [[ -L "$file" || ! -f "$file" ]]; then
    printf 'Leaving non-regular shell configuration untouched: %s\n' "$file" >&2
    return 0
  fi
  temp="$(mktemp "${file}.rustovan.XXXXXX")"
  if ! awk -v start="$PROFILE_START" -v finish="$PROFILE_END" '
    $0 == start { if (inside) exit 3; inside=1; next }
    $0 == finish { if (!inside) exit 3; inside=0; next }
    !inside { print }
    END { if (inside) exit 3 }
  ' "$file" > "$temp"; then
    rm -f "$temp"
    printf 'Malformed Rustovan PATH markers; leaving %s unchanged.\n' "$file" >&2
    return 1
  fi
  if cmp -s "$file" "$temp"; then
    rm -f "$temp"
  else
    copy_file_mode "$file" "$temp"
    mv -f "$temp" "$file"
  fi
}

add_profile_block() {
  local file="$1" block="$2" temp
  if [[ -L "$file" || ( -e "$file" && ! -f "$file" ) ]]; then
    printf 'Leaving non-regular shell configuration untouched: %s\n' "$file" >&2
    return 0
  fi
  [[ -e "$file" ]] || (umask 077; : > "$file")
  temp="$(mktemp "${file}.rustovan.XXXXXX")"
  if ! awk -v start="$PROFILE_START" -v finish="$PROFILE_END" '
    $0 == start { if (inside) exit 3; inside=1; next }
    $0 == finish { if (!inside) exit 3; inside=0; next }
    !inside { print }
    END { if (inside) exit 3 }
  ' "$file" > "$temp"; then
    rm -f "$temp"
    printf 'Malformed Rustovan PATH markers; leaving %s unchanged.\n' "$file" >&2
    return 1
  fi
  {
    printf '\n%s\n' "$PROFILE_START"
    printf '%s\n' "$block"
    printf '%s\n' "$PROFILE_END"
  } >> "$temp"
  copy_file_mode "$file" "$temp"
  mv -f "$temp" "$file"
}

remove_path_configuration() {
  local file fish_file
  for file in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.zprofile" "$HOME/.zshrc"; do
    remove_profile_block "$file" || return 1
  done
  fish_file="$HOME/.config/fish/conf.d/rustovan-path.fish"
  remove_profile_block "$fish_file" || return 1
  if [[ -f "$fish_file" && ! -s "$fish_file" ]]; then rm -f "$fish_file"; fi
}

if [[ "$action" == uninstall ]]; then
  if [[ -L "$command_dir" ]]; then
    printf 'Refusing to remove files through symlinked command directory: %s\n' "$command_dir" >&2
  elif [[ -L "$command_path" ]]; then
    printf 'Refusing to remove symlink at %s.\n' "$command_path" >&2
  elif is_safe_file "$command_path" && head -n 2 "$command_path" | grep -Fq "$WRAPPER_MARKER"; then
    rm -f "$command_path"
  elif [[ -e "$command_path" ]]; then
    printf 'Leaving unmanaged command file untouched: %s\n' "$command_path" >&2
  fi
  remove_path_configuration
  if [[ -L "$data_root" || -L "$app_dir" ]]; then
    printf 'Refusing to remove symlinked application directory: %s\n' "$app_dir" >&2
  elif [[ -d "$app_dir" && -f "$marker_file" && ! -L "$marker_file" ]] && grep -Fxq 'rustovan-managed-v1' "$marker_file"; then
    for file in "$installed_binary" "$metadata_file" "$marker_file"; do
      if [[ -L "$file" ]]; then
        printf 'Leaving symlink untouched: %s\n' "$file" >&2
      elif [[ -f "$file" ]]; then
        rm -f "$file"
      fi
    done
    rmdir "$app_dir" 2>/dev/null || printf 'Preserved non-empty directory: %s\n' "$app_dir"
    rmdir "$data_root" 2>/dev/null || true
  elif [[ -e "$app_dir" ]]; then
    printf 'Leaving unmanaged application directory untouched: %s\n' "$app_dir" >&2
  fi
  printf 'Rustovan uninstall finished. Open a new terminal to refresh PATH.\n'
  exit 0
fi

os_name="$(uname -s)"
machine="$(uname -m)"
if [[ "$os_name" == Darwin && "$machine" == x86_64 ]] && command -v sysctl >/dev/null 2>&1 && [[ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" == 1 ]]; then
  machine=arm64
fi
if [[ "$os_name" == MINGW* || "$os_name" == MSYS* || "$os_name" == CYGWIN* ]]; then
  echo 'Use install.ps1 from PowerShell on Windows.' >&2
  exit 1
fi

is_termux=false
if [[ "${PREFIX:-}" == *com.termux* || -n "${TERMUX_VERSION:-}" || -n "${ANDROID_ROOT:-}" ]]; then
  is_termux=true
fi
if [[ -n "${ANDROID_ROOT:-}" && "${PREFIX:-}" != *com.termux* && -z "${TERMUX_VERSION:-}" ]]; then
  echo 'Android installs are supported through Termux only; this is not a generic Android package.' >&2
  exit 1
fi

asset_id=''
case "$os_name:$machine" in
  Darwin:x86_64|Darwin:amd64) asset_id=macos-x86_64 ;;
  Darwin:arm64|Darwin:aarch64) asset_id=macos-aarch64 ;;
  Linux:*)
    if [[ "$is_termux" == true ]]; then
      case "$machine" in
        aarch64|arm64) asset_id=android-termux-aarch64 ;;
        *) printf 'Termux release binaries are currently available only for ARM64 (detected %s).\n' "$machine" >&2; exit 1 ;;
      esac
    else
      is_musl=false
      if [[ -e /etc/alpine-release ]]; then
        is_musl=true
      elif command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | head -n 2 | grep -qi musl; then
        is_musl=true
      fi
      case "$machine:$is_musl" in
        x86_64:false|amd64:false) asset_id=linux-amd64 ;;
        aarch64:false|arm64:false) asset_id=linux-aarch64 ;;
        armv7l:false|armv7:false) asset_id=linux-armv7 ;;
        riscv64:false) asset_id=linux-riscv64 ;;
        x86_64:true|amd64:true) asset_id=linux-musl-amd64 ;;
        aarch64:true|arm64:true) asset_id=linux-musl-aarch64 ;;
        *) printf 'No published release target matches Linux %s (musl=%s).\n' "$machine" "$is_musl" >&2; exit 1 ;;
      esac
    fi
    ;;
  *) printf 'Unsupported operating system or architecture: %s %s\n' "$os_name" "$machine" >&2; exit 1 ;;
esac

for dependency in curl python3 tar; do
  if ! command -v "$dependency" >/dev/null 2>&1; then
    printf 'Required command not found: %s\n' "$dependency" >&2
    if [[ "$is_termux" == true ]]; then
      echo 'On Termux, install prerequisites with: pkg install curl python tar coreutils' >&2
    else
      echo 'Install curl, Python 3, and tar, then run this installer again.' >&2
    fi
    exit 1
  fi
done

if [[ -L "$command_dir" || ( -e "$command_dir" && ! -d "$command_dir" ) ]]; then
  printf 'Command directory is not a regular directory: %s\n' "$command_dir" >&2
  exit 1
fi
if [[ -L "$command_path" ]]; then
  printf 'Refusing to replace symlink at %s.\n' "$command_path" >&2
  exit 1
fi
if [[ -e "$command_path" ]] && { ! is_safe_file "$command_path" || ! head -n 2 "$command_path" | grep -Fq "$WRAPPER_MARKER"; }; then
  printf 'Refusing to replace unmanaged command file: %s\n' "$command_path" >&2
  exit 1
fi

github_token="${RUSTOVAN_GITHUB_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
if [[ -z "$github_token" ]] && command -v gh >/dev/null 2>&1; then
  github_token="$(gh auth token 2>/dev/null || true)"
fi
if [[ -n "$github_token" && ! "$github_token" =~ ^[A-Za-z0-9_]+$ ]]; then
  echo 'GitHub token contains unsupported characters; refusing to put it in an HTTP header.' >&2
  exit 1
fi

api_get() {
  local url="$1"
  if [[ -n "$github_token" ]]; then
    printf 'silent\nshow-error\nfail\nheader = "Accept: application/vnd.github+json"\nheader = "Authorization: Bearer %s"\nheader = "X-GitHub-Api-Version: 2022-11-28"\n' "$github_token" | curl --config - --url "$url"
  else
    curl -fsS -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' "$url"
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print tolower($1)}'
  else echo 'Neither sha256sum nor shasum is available.' >&2; return 1
  fi
}

temp_root="$(mktemp -d "${TMPDIR:-/tmp}/rustovan-install.XXXXXX")"
cleanup() { rm -rf "$temp_root"; }
trap cleanup EXIT INT TERM
release_json="$temp_root/release.json"
if ! api_get "$API_ROOT/releases/latest" > "$release_json"; then
  cat >&2 <<'ERR'
Could not read the latest Rustovan GitHub release. The repository may be private
or rate-limited. For a private repository, set RUSTOVAN_GITHUB_TOKEN to a
GitHub token with read access to releases (or set GH_TOKEN/GITHUB_TOKEN).
ERR
  exit 1
fi
tag="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["tag_name"])' "$release_json")"
if [[ ! "$tag" =~ ^[A-Za-z0-9._-]+$ ]]; then
  printf 'Unexpected release tag format: %s\n' "$tag" >&2
  exit 1
fi

find_asset_id() {
  python3 -c 'import json,sys
name=sys.argv[2]
for a in json.load(open(sys.argv[1], encoding="utf-8")).get("assets", []):
    if a.get("name") == name:
        print(a["id"])
        break' "$release_json" "$1"
}

archive_name="rustovan-${asset_id}.zip"
archive_member='rustovan'
if [[ "$os_name" == Darwin || "$os_name" == Linux ]]; then archive_name="rustovan-${asset_id}.tar.gz"; fi
package_asset_id="$(find_asset_id "$archive_name")"
if [[ -z "$package_asset_id" ]]; then
  archive_name="ton-vanity-rust-${asset_id}.tar.gz"
  archive_member='ton-vanity-rust'
  package_asset_id="$(find_asset_id "$archive_name")"
fi
if [[ -z "$package_asset_id" && "$asset_id" == linux-amd64 ]]; then
  archive_name=ton-vanity-rust-linux-x86_64.tar.gz
  archive_member='ton-vanity-rust'
  package_asset_id="$(find_asset_id "$archive_name")"
fi
if [[ -z "$package_asset_id" ]]; then
  printf 'Latest release %s does not contain the required asset %s.\n' "$tag" "$archive_name" >&2
  exit 1
fi
checksums_asset_id="$(find_asset_id SHA256SUMS.txt)"
if [[ -z "$checksums_asset_id" ]]; then
  printf 'Latest release %s has no SHA256SUMS.txt asset; refusing an unverifiable download.\n' "$tag" >&2
  exit 1
fi

fetch_asset() {
  local id="$1" destination="$2" headers status location
  headers="$temp_root/headers"
  rm -f "$headers" "$destination"
  if [[ -n "$github_token" ]]; then
    printf 'silent\nshow-error\nfail\nheader = "Accept: application/octet-stream"\nheader = "Authorization: Bearer %s"\nheader = "X-GitHub-Api-Version: 2022-11-28"\n' "$github_token" | curl --config - --dump-header "$headers" --output "$destination" "$API_ROOT/releases/assets/$id"
  else
    curl -fsS -H 'Accept: application/octet-stream' -H 'X-GitHub-Api-Version: 2022-11-28' --dump-header "$headers" --output "$destination" "$API_ROOT/releases/assets/$id"
  fi
  status="$(awk '/^HTTP\// { code=$2 } END { print code }' "$headers")"
  if [[ "$status" =~ ^30[12378]$ ]]; then
    location="$(awk 'tolower($1)=="location:" { sub(/\r$/, "", $2); value=$2 } END { print value }' "$headers")"
    if [[ -z "$location" || "$location" != https://* ]]; then
      echo 'GitHub returned an invalid or non-HTTPS asset redirect.' >&2
      return 1
    fi
    # The bearer token is never sent to the signed CDN redirect.
    curl -fsSL --proto '=https' --proto-redir '=https' "$location" -o "$destination"
  elif [[ "$status" != 200 ]]; then
    printf 'Unexpected GitHub asset response: HTTP %s\n' "${status:-unknown}" >&2
    return 1
  fi
  [[ -s "$destination" ]] || { echo 'Downloaded release asset is empty.' >&2; return 1; }
}

checksum_file="$temp_root/SHA256SUMS.txt"
fetch_asset "$checksums_asset_id" "$checksum_file"
package_sha="$(awk -v name="$archive_name" '$2==name || $2=="*" name { print tolower($1); exit }' "$checksum_file")"
if [[ ! "$package_sha" =~ ^[a-f0-9]{64}$ ]]; then
  printf 'No valid SHA-256 entry for %s in the release manifest.\n' "$archive_name" >&2
  exit 1
fi

metadata_value() { sed -n "s/^$1=//p" "$metadata_file" 2>/dev/null | head -n 1; }
already_current=false
if [[ -d "$app_dir" && ! -L "$app_dir" && ! -L "$data_root" && -f "$marker_file" && ! -L "$marker_file" ]] && grep -Fxq 'rustovan-managed-v1' "$marker_file" && is_safe_file "$installed_binary" && is_safe_file "$metadata_file"; then
  old_tag="$(metadata_value tag)"
  old_asset="$(metadata_value asset)"
  old_package_sha="$(metadata_value package_sha256)"
  old_binary_sha="$(metadata_value binary_sha256)"
  actual_binary_sha="$(sha256_file "$installed_binary")"
  if [[ "$old_tag" == "$tag" && "$old_asset" == "$asset_id" && "$old_package_sha" == "$package_sha" && "$old_binary_sha" == "$actual_binary_sha" ]]; then
    already_current=true
  fi
fi

if [[ "$already_current" == true ]]; then
  printf 'Rustovan %s (%s) is already installed and verified; no binary download needed.\n' "$tag" "$asset_id"
else
  archive_file="$temp_root/$archive_name"
  fetch_asset "$package_asset_id" "$archive_file"
  actual_package_sha="$(sha256_file "$archive_file")"
  if [[ "$actual_package_sha" != "$package_sha" ]]; then
    echo 'Release archive SHA-256 does not match SHA256SUMS.txt; refusing installation.' >&2
    exit 1
  fi
  if [[ -L "$data_root" || ( -e "$data_root" && ! -d "$data_root" ) || -L "$app_dir" || ( -e "$app_dir" && ! -d "$app_dir" ) ]]; then
    printf 'Refusing to install through a non-directory or symlinked path: %s\n' "$app_dir" >&2
    exit 1
  fi
  if [[ -e "$app_dir" && ! -f "$marker_file" ]]; then
    printf 'Install directory exists but is not managed by Rustovan: %s\n' "$app_dir" >&2
    exit 1
  fi
  if [[ -L "$marker_file" ]]; then
    printf 'Refusing to follow a symlinked install marker: %s\n' "$marker_file" >&2
    exit 1
  fi
  mkdir -p "$app_dir"
  if [[ -L "$installed_binary" || ( -e "$installed_binary" && ! -f "$installed_binary" ) ]]; then
    printf 'Refusing to replace non-regular installed binary: %s\n' "$installed_binary" >&2
    exit 1
  fi
  staging_binary="$(mktemp "$app_dir/.rustovan.XXXXXX")"
  if [[ "$archive_name" == *.tar.gz ]]; then
    members="$(tar -tzf "$archive_file")"
    types="$(tar -tvzf "$archive_file" | awk '{print substr($0,1,1)}')"
    if [[ "$members" != "$archive_member" || "$types" != - ]]; then
      printf 'Unix release archive must contain exactly one regular file named %s.\n' "$archive_member" >&2
      exit 1
    fi
    tar -xOzf "$archive_file" "$archive_member" > "$staging_binary"
  else
    echo 'The selected release asset has an unexpected archive format.' >&2
    exit 1
  fi
  chmod 755 "$staging_binary"
  binary_sha="$(sha256_file "$staging_binary")"
  mv -f "$staging_binary" "$installed_binary"
  printf 'rustovan-managed-v1\n' > "$marker_file"
  metadata_temp="$(mktemp "$app_dir/.install.properties.XXXXXX")"
  {
    printf 'tag=%s\n' "$tag"
    printf 'asset=%s\n' "$asset_id"
    printf 'package_sha256=%s\n' "$package_sha"
    printf 'binary_sha256=%s\n' "$binary_sha"
  } > "$metadata_temp"
  mv -f "$metadata_temp" "$metadata_file"
  printf 'Installed Rustovan %s for %s.\n' "$tag" "$asset_id"
fi

if [[ -L "$command_dir" || ( -e "$command_dir" && ! -d "$command_dir" ) ]]; then
  printf 'Command directory is not a regular directory: %s\n' "$command_dir" >&2
  exit 1
fi
mkdir -p "$command_dir"
if [[ -L "$command_path" ]]; then
  printf 'Refusing to replace symlink at %s.\n' "$command_path" >&2
  exit 1
fi
if [[ -e "$command_path" ]] && { ! is_safe_file "$command_path" || ! head -n 2 "$command_path" | grep -Fq "$WRAPPER_MARKER"; }; then
  printf 'Refusing to replace unmanaged command file: %s\n' "$command_path" >&2
  exit 1
fi
quoted_binary="$(printf '%s' "$installed_binary" | sed "s/'/'\\\\''/g")"
wrapper_temp="$(mktemp "$command_dir/.rustovan.XXXXXX")"
{
  printf '#!/bin/sh\n%s\nexec '\''%s'\'' "$@"\n' "$WRAPPER_MARKER" "$quoted_binary"
} > "$wrapper_temp"
chmod 755 "$wrapper_temp"
mv -f "$wrapper_temp" "$command_path"

path_block='case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac'
for profile in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.zprofile" "$HOME/.zshrc"; do
  add_profile_block "$profile" "$path_block"
done
if command -v fish >/dev/null 2>&1; then
  fish_dir="$HOME/.config/fish/conf.d"
  fish_file="$fish_dir/rustovan-path.fish"
  if [[ -L "$fish_dir" || ( -e "$fish_dir" && ! -d "$fish_dir" ) || -L "$fish_file" ]]; then
    printf 'Cannot safely add Fish PATH configuration at %s.\n' "$fish_file" >&2
  else
    mkdir -p "$fish_dir"
    fish_block='if not contains -- $HOME/.local/bin $PATH
    set -gx PATH $HOME/.local/bin $PATH
end'
    add_profile_block "$fish_file" "$fish_block"
  fi
fi

printf 'Global command installed: %s\n' "$command_path"
printf 'Open a new terminal, then run: rustovan --help\n'
printf 'To update later: bash install.sh update\nTo uninstall: bash install.sh uninstall\n'
