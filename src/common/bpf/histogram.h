// Helpers for converting values to histogram indices. 

#define HISTOGRAM_BUCKETS_POW_4 976
#define HISTOGRAM_BUCKETS_POW_5 1920
#define HISTOGRAM_BUCKETS_POW_6 3776
#define HISTOGRAM_BUCKETS_POW_7 7424

// Function to count leading zeros, since we cannot use the builtin CLZ from
// within BPF. But since we also can't loop, this is implemented as a binary
// search with a maximum of 6 branches. 
static u32 clz(u64 value) {
    u32 count = 0;

    // binary search to find number of leading zeros
    if (value & 0xFFFFFFFF00000000) {
        if (value & 0xFFFF000000000000) {
            if (value & 0xFF00000000000000) {
                if (value & 0xF000000000000000) {
                    if (value & 0xC000000000000000) {
                        if (value & 0x8000000000000000) {
                            return 0;
                        } else {
                            return 1;
                        }
                    } else if (value & 0x2000000000000000) {
                        return 2;
                    } else {
                        return 3;
                    }
                } else if (value & 0x0C00000000000000) {
                    if (value & 0x0800000000000000) {
                        return 4;
                    } else {
                        return 5;
                    }
                } else if (value & 0x0200000000000000) {
                    return 6;
                } else {
                    return 7;
                }
            } else if (value & 0x00F0000000000000) {
                if (value & 0x00C0000000000000) {
                    if (value & 0x0080000000000000) {
                        return 8;
                    } else {
                        return 9;
                    }
                } else if (value & 0x0020000000000000) {
                    return 10;
                } else {
                    return 11;
                }
            } else if (value & 0x000C000000000000) {
                if (value & 0x0008000000000000) {
                    return 12;
                } else {
                    return 13;
                }
            } else if (value & 0x0002000000000000) {
                return 14;
            } else {
                return 15;
            }
        } else if (value & 0x0000FF0000000000) {
            if (value & 0x0000F00000000000) {
                if (value & 0x0000C00000000000) {
                    if (value & 0x0000800000000000) {
                        return 16;
                    } else {
                        return 17;
                    }
                } else if (value & 0x0000200000000000) {
                    return 18;
                } else {
                    return 19;
                }
            } else if (value & 0x00000C0000000000) {
                if (value & 0x0000080000000000) {
                    return 20;
                } else {
                    return 21;
                }
            } else if (value & 0x0000020000000000) {
                return 22;
            } else {
                return 23;
            }
        } else if (value & 0x000000F000000000) {
            if (value & 0x000000C000000000) {
                if (value & 0x0000008000000000) {
                    return 24;
                } else {
                    return 25;
                }
            } else if (value & 0x0000002000000000) {
                return 26;
            } else {
                return 27;
            }
        } else if (value & 0x0000000C00000000) {
            if (value & 0x0000000800000000) {
                return 28;
            } else {
                return 29;
            }
        } else if (value & 0x0000000200000000) {
            return 30;
        } else {
            return 31;
        }
    } else if (value & 0x00000000FFFF0000) {
        if (value & 0x00000000FF000000) {
            if (value & 0x00000000F0000000) {
                if (value & 0x00000000C0000000) {
                    if (value & 0x0000000080000000) {
                        return 32;
                    } else {
                        return 33;
                    }
                } else if (value & 0x0000000020000000) {
                    return 34;
                } else {
                    return 35;
                }
            } else if (value & 0x000000000C000000) {
                if (value & 0x0000000008000000) {
                    return 36;
                } else {
                    return 37;
                }
            } else if (value & 0x0000000002000000) {
                return 38;
            } else {
                return 39;
            }
        } else if (value & 0x0000000000F00000) {
            if (value & 0x0000000000C00000) {
                if (value & 0x0000000000800000) {
                    return 40;
                } else {
                    return 41;
                }
            } else if (value & 0x0000000000200000) {
                return 42;
            } else {
                return 43;
            } 
        } else if (value & 0x00000000000C0000) {
            if (value & 0x0000000000080000) {
                return 44;
            } else {
                return 45;
            }
        } else if (value & 0x0000000000020000) {
            return 46;
        } else {
            return 47;
        }
    } else if (value & 0x000000000000FF00) {
        if (value & 0x000000000000F000) {
            if (value & 0x000000000000C000) {
                if (value & 0x0000000000008000) {
                    return 48;
                } else {
                    return 49;
                }
            } else if (value & 0x000000000002000) {
                return 50;
            } else {
                return 51;
            }
        } else if (value & 0x0000000000000C00) {
            if (value & 0x0000000000000800) {
                return 52;
            } else {
                return 53;
            }
        } else if (value & 0x0000000000000200) {
            return 54;
        } else {
            return 55;
        }
    } else if (value & 0x00000000000000F0) {
        if (value & 0x00000000000000C0) {
            if (value & 0x0000000000000080) {
                return 56;
            } else {
                return 57;
            }
        } else if (value & 0x0000000000000020) {
            return 58;
        } else {
            return 59;
        }
    } else if (value & 0x000000000000000C) {
        if (value & 0x0000000000000008) {
            return 60;
        } else {
            return 61;
        }
    } else if (value & 0x0000000000000002) {
        return 62;
    } else {
        return 63;
    }

    return 64;
}

// base-2 histogram indexing function that is compatible with Rust `histogram`
// crate.
//
// See the indexing logic here:
// https://github.com/pelikan-io/rustcommon/blob/main/histogram/src/config.rs
static u32 value_to_index(u64 value, u8 grouping_power) {
    if (value < (2 << grouping_power)) {
        return value;
    } else {
        u64 power = 63 - clz(value);
        u64 bin = power - grouping_power + 1;
        u64 offset = (value - (1 << power)) >> (power - grouping_power);

        return (bin * (1 << grouping_power) + offset);
    }
}
