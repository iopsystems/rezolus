#!/usr/bin/env python3
"""Print a read-only inventory of Linux platform and sensor data as JSON."""

import glob
import json
import platform
from datetime import datetime, timezone
from pathlib import Path


def read(path, errors=None):
    try:
        return Path(path).read_text().replace("\0", "\n").strip()
    except OSError as error:
        if errors is not None:
            errors[str(path)] = str(error)
        return None


def inventory(pattern, attributes, errors=None):
    result = []
    for name in sorted(glob.glob(pattern)):
        directory = Path(name)
        values = {}
        for attribute in attributes:
            for path in sorted(directory.glob(attribute)):
                if path.is_file():
                    values[path.name] = read(path, errors)
        result.append({
            "path": name,
            "resolved_path": str(directory.resolve()),
            "device_path": (
                str((directory / "device").resolve())
                if (directory / "device").exists() else None
            ),
            "attributes": values,
        })
    return result


if __name__ == "__main__":
    errors = {}
    print(json.dumps({
        "captured_at": datetime.now(timezone.utc).isoformat(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "model": read("/sys/firmware/devicetree/base/model", errors),
        "compatible": read("/sys/firmware/devicetree/base/compatible", errors),
        "l4t_release": read("/etc/nv_tegra_release", errors),
        "thermal_zones": inventory("/sys/class/thermal/thermal_zone*", [
            "type", "temp", "mode", "policy", "trip_point_*",
        ], errors),
        "cooling_devices": inventory("/sys/class/thermal/cooling_device*", [
            "type", "cur_state", "max_state",
        ], errors),
        "hwmon": inventory("/sys/class/hwmon/hwmon*", [
            "name", "temp*", "power[0-9]*", "in[0-9]*",
            "curr[0-9]*", "fan[0-9]*", "pwm[0-9]*", "rpm",
            "oc*_event_cnt", "oc*_throt_en", "samples", "update_interval",
            "shunt*_resistor",
        ], errors),
        "read_errors": errors,
    }, indent=2))
