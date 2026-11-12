# Corsair AXi power supplies in hw_sensors

Status: implemented, tested on one AX1500i (hv02, Threadripper 1950X host).

## Goal

Record wall power for hosts with a Corsair AXi PSU. RAPL measures CPU package,
cores, GPU, DRAM and, on some Intel parts, the platform, not the machine at the
wall; the PSU's `READ_PIN` is the whole machine at the wall. The
kernel's `corsair-psu` hwmon driver covers HXi/RMi (USB IDs `1c03`–`1c27`), not
the AXi line, whose Corsair Link dongle is a CP210x USB-serial bridge
(`1b1c:1c02`) bound to `cp210x`.

## Protocol source

Corsair does not document the dongle. The framing and command bytes come from
the community `cpsumon` project, read as a description only (it is GPL-2; no
code was taken). Every detail used was checked against captures from the
AX1500i, committed as `src/agent/samplers/hw_sensors/linux/fixtures/corsair-axi-ax1500i-{handshake,load}.jsonl`
(timestamped tx/rx bytes; the load capture spans a 32-thread CPU load step).

Findings from the captures:

- Replies vary in length: the dongle's name reply is 90 bytes, a two-byte
  register reply 6. The load capture read replies by expected length and lost
  alignment after the name reply. Reassembled into frames, it holds 734
  replies for 735 requests; the missing one is the acknowledgement of the first
  PAGE request. I don't know whether the capture lost it or the dongle sent
  none. A missing reply fails the open, and discovery retries 60 s later. Frames are read to their `0x00` terminator,
  which cannot occur inside a frame (code bytes are `0x55`–`0xaa`).
- `READ_VIN` × `READ_IIN` agrees with `READ_PIN` within 3.1% on each of the
  65 polls in the two captures where all three replies decoded, with `READ_PIN` from 184 W idle to 371 W under load.
- Register `0xee`, which `cpsumon` labels output power, read 0 on most polls
  of the load capture and 346–348 W on a few, while `READ_PIN` read a value on
  every poll. NO-GO: not published. Reopen
  if a capture shows it tracking `READ_PIN` minus losses.
- The fan read 0 rpm in every capture, up to 371 W input.
- PAGE read 0. The session refuses a PSU on another page rather than writing
  PAGE, so the agent writes no PMBus register.

## Port contention

On hv02, CyberPower's `pwrstatd` (installed, no UPS attached) had
`/dev/ttyUSB0` open and consumed the dongle's replies. With it running, the
agent's discovery reports `corsair_axi 1-13 (/dev/ttyUSB0): corsair_axi: no
reply`, and the sensors are absent. hv02's `powerpanel` package was then
removed.

The first version backed off only failed opens. Review found that a register
that never answers, with every other register answering, cost a 250 ms
timeout and a reopen on every sweep, since each reopen succeeded. Failures are
now held back per register (1 s, doubling to 60 s) as well as per port (60 s
after a failed open, for readings; discovery retries on its own interval).

The port is now opened with `TIOCEXCL` and a `flock`, and the tty is checked in
sysfs to still be the dongle before each open, since `ttyUSBn` numbers are
reassigned on replug. A second review found that a root process can open a
port another process has set `TIOCEXCL` on, and the packaged agent runs as
root; the agent now reads `TIOCGEXCL` and the lock before changing the port's
settings, so it refuses such a port without disturbing it. Discovery closes
sessions for ports it no longer finds, and closing a port discards unsent
output first (usb-serial otherwise waits up to 30 s for it to drain). Not
handled: two dongles whose `ttyUSBn` numbers swap between rediscoveries; the
sysfs check confirms the USB IDs, not which dongle, so readings can be
published under the other PSU's identity for up to 60 s. The PMBus page is reread before a reading at most once a
second, because another tool could change it (cpsumon's `read_psu_railmisc`
leaves the PSU on page 2) between readings without breaking a session. A page
other than 0 counts as a failed open, not as a failed register.

On Linux, a tty in raw mode with VMIN 0 and `O_NONBLOCK` returns 0 from
`read` when no byte is waiting, not `EAGAIN`; a hangup is visible only as
`POLLHUP`.

## Cost

Measured on hv02 with the first version's release build, `hw_sensors` alone at 5 s,
`pwrstatd` stopped: sweeps of 12 values took 29.6–29.9 ms (3 sweeps), against
127–171 µs for the same host's 6 hwmon values with the PSU unreadable.
Discovery took 46 ms with the handshake, 259 ms with the open timing out. The
sweep time is serial round trips (two exchanges per register) on the blocking
worker, not the scrape path. Agent CPU was not measured separately. Exchange
times in the load capture (median 12.3 ms) include the capture script's own
polling; the handshake capture's median is 2.5 ms.

After the review fixes (page reread once a second, exclusive open), the same
setup gave sweeps of 34.3–35.2 ms (12 sweeps over 60 s; the first, with the
page checked at open, 29.8 ms), a discovery that opened the port 46 ms, and a
rediscovery with the port already open 2.5 ms.

## Not done

- AX1600i (hv01): not yet cabled or captured; whether its dongle has the
  same USB ID is unknown. The kernel's `cp210x` table also lists `1b1c:1c00`
  ("Corsair USB Dongle"), which this change does not match.
- AX760i/AX860i/AX1200i: likely the same protocol; no hardware to capture.
