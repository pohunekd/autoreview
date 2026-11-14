---
kind: changed
---
Two operator messages state their limit without naming the removed Go engine: a stored time with a year outside 0..=9999 is refused as outside the years the stored time JSON form holds, and `scripts/env-to-config` names the largest accepted value when it refuses a duration that 64-bit nanoseconds cannot hold.
