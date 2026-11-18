# Sourced by the justfile's firmware recipes: the ESP toolchain, then the build's secrets.
# Anything already exported wins. The rest come from THERMO_SECRETS, if set: a sops file with
# wifi_ssid, wifi_password and thermo_master. QUESTDB_ADDR and THERMO_GATEWAY (the gateway's
# address, for ota) are plain settings: export them yourself.
[ -f ~/export-esp.sh ] && . ~/export-esp.sh
if [ -n "${THERMO_SECRETS:-}" ]; then
    : "${WIFI_SSID:=$(sops -d --extract '["wifi_ssid"]' "$THERMO_SECRETS")}"
    : "${WIFI_PASSWORD:=$(sops -d --extract '["wifi_password"]' "$THERMO_SECRETS")}"
    : "${THERMO_MASTER:=$(sops -d --extract '["thermo_master"]' "$THERMO_SECRETS")}"
fi
export WIFI_SSID WIFI_PASSWORD THERMO_MASTER
