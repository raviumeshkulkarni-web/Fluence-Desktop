#!/usr/bin/env python3
"""
Convergence corpus generator — frozen v1.2.1 wire contract.
Deterministic (fixed random.seed list). Generates 30 scenarios.

Reference semantics (EXACT):
- Dictionary/snippets: identity = spoken.trim().lowercase() / trigger.trim().lowercase().
  Winner per identity = max(updatedAt), tie -> lexicographically max(deviceId).
  Tombstones (deletedAt != null) are ordinary records.
  Final sort: businessKey then syncId.
- Settings (5 keys only): per key, candidates -> winner rule:
    if exactly one has updatedAt==0 the other wins;
    both 0 -> max deviceId;
    else max updatedAt (tie -> max deviceId).
  THEN if winner.updatedAt == 0 set it to 1700000000000.
  Sort by key.
- Stats: union by eventId (no conflict). Sort: day then eventId.
- Envelope bytes: compact JSON {"v":1,"entries":[...]} + trailing newline,
  keys in contract order (handled by production code, not generator).

Output: scenario-NN.json pretty-printed (indent=2) into:
  WINDOWS/examples/sync/convergence/
  ANDROID/app/src/test/resources/convergence/  (byte-identical)
"""
import random
import uuid
import json
import pathlib
import datetime

FIXED_STAMP = 1700000000000
ALLOWED_SETTINGS = ["language", "dictionary_enabled", "snippets_enabled", "auto_learn_enabled", "ai_polish_style"]

# Deterministic seeds — fixed list per harness requirement
FIXED_SEEDS = [20260826 + i * 10007 for i in range(30)]  # 30 seeds, deterministic

WINDOWS_CONV = pathlib.Path(__file__).parent
ANDROID_CONV = pathlib.Path(r"D:\Working files\Fluence trasncribe\Android\app\src\test\resources\convergence")

# Pools for adversarial generation
DICT_SPOKEN_VARIANTS = [
    "hello", " Hello", "HELLO ", "  hello  ", "HeLLo",
    "gonna", "Gonna ", " gonna", "GONNA", "  gonna  ",
    "teh", " Teh ", "TEH", "teh ", "  TEH  ",
    "github", "GitHub", " GITHUB ", "gitHub ", "  github",
    "नमस्ते", " नमस्ते ", "नमस्ते ", "  नमस्ते  ",  # Hindi
    "😀-test", "😀-Test ", " 😀-test", "😀-TEST",
    "привет", " Привет ", "ПРИВЕТ", " привет ",
    "afternoon", "Afternoon ", " AFTERNOON", "  afternoon  ",
    "thanks", " Thanks", "THANKS ", "  thanks  ",
    "coffee", " Coffee ", "COFFEE",
]

DICT_CORRECTED_POOL = [
    "hello", "going to", "the", "GitHub", "Namaste", "😀-test fixed",
    "привет-fixed", "afternoon", "thank you", "coffee", "address",
    "be right back", "as soon as possible", "example", "test",
    "  keep   internal spaces\tand tabs ", "Corrected Text", "FIXED",
]

SNIPPET_TRIGGER_VARIANTS = [
    "addr", " Addr ", "ADDR", "  addr  ",
    "brb", " Brb", "BRB ", "  brb  ",
    "sig", " Sig ", "SIG", "sig ",
    "नमस्ते", " नमस्ते ", "привет", "😀-sig", "😀-Sig ",
    "thanks", " ty ", "TY", "omw", " OMW ", "omw ",
    "afk", " AFK ", "g2g", " G2G ",
]

SNIPPET_EXPANSION_POOL = [
    "123 Example Street, Springfield",
    "be right back", "Best regards,\nAlex",
    "On my way!", "Thanks!", "  preserve   spaces  ",
    "नमस्ते expansion", "😀 expansion with emoji", "привет expansion",
    "See you soon", "Got to go", "Away from keyboard",
]

SETTINGS_VALUES = {
    "language": ["en", "de", "fr", "es", "hi", "ru", "ja"],
    "dictionary_enabled": ["true", "false"],
    "snippets_enabled": ["true", "false"],
    "auto_learn_enabled": ["true", "false"],
    "ai_polish_style": ["default", "formal", "concise", "bullet", "translate"],
}

# For stats
DAYS_POOL = ["2026-01-01", "2026-01-02", "2026-02-15", "2026-08-20", "2026-08-21", "2026-07-10", "2026-03-03", "2026-12-31", "2026-06-15"]

def det_uuid():
    """Deterministic UUIDv4 from random bits (not os.urandom)."""
    return str(uuid.UUID(int=random.getrandbits(128), version=4))

def business_key_dict(spoken: str) -> str:
    return spoken.strip().lower()

def business_key_snip(trigger: str) -> str:
    return trigger.strip().lower()

def reference_merge_dictionary(all_items):
    """all_items: list of dict items (with spoken etc). Returns winner list sorted businessKey then syncId."""
    grouped = {}
    for it in all_items:
        bk = business_key_dict(it["spoken"])
        grouped.setdefault(bk, []).append(it)
    winners = []
    for bk, candidates in grouped.items():
        # max by (updatedAt, deviceId)
        winner = max(candidates, key=lambda x: (x["updatedAt"], x["deviceId"]))
        winners.append(winner)
    # sort by businessKey then syncId
    winners.sort(key=lambda x: (business_key_dict(x["spoken"]), x["syncId"]))
    # Add businessKey field for expected (recomputed)
    for w in winners:
        w["businessKey"] = business_key_dict(w["spoken"])
    return winners

def reference_merge_snippets(all_items):
    grouped = {}
    for it in all_items:
        bk = business_key_snip(it["trigger"])
        grouped.setdefault(bk, []).append(it)
    winners = []
    for bk, candidates in grouped.items():
        winner = max(candidates, key=lambda x: (x["updatedAt"], x["deviceId"]))
        winners.append(winner)
    winners.sort(key=lambda x: (business_key_snip(x["trigger"]), x["syncId"]))
    for w in winners:
        w["businessKey"] = business_key_snip(w["trigger"])
    return winners

def reference_merge_settings(all_items):
    """all_items: list of settings ops with key,value,updatedAt,deviceId. Returns winners sorted by key, with t=0 handling."""
    # group per key, filter allowed only
    grouped = {}
    for it in all_items:
        if it["key"] not in ALLOWED_SETTINGS:
            continue
        grouped.setdefault(it["key"], []).append(it)
    winners = []
    for key, candidates in grouped.items():
        # winner rule with t=0 special
        # if exactly one has updatedAt==0 the other wins; both 0 -> max deviceId; else max updatedAt tie->max deviceId
        # To implement, sort candidates by custom comparator
        def winner_key(it):
            # We need to choose winner per rule, not simple max. We'll iterate pairwise.
            return (it["updatedAt"], it["deviceId"])
        # Find winner via pairwise comparison using rule
        winner = candidates[0]
        for cand in candidates[1:]:
            # Compare cand vs winner using t=0 rule
            # Rule: if exactly one has t==0, the other wins
            a, b = cand, winner
            a_zero = a["updatedAt"] == 0
            b_zero = b["updatedAt"] == 0
            if a_zero and not b_zero:
                # b wins, keep winner
                continue
            elif b_zero and not a_zero:
                # a wins
                winner = a
            elif a_zero and b_zero:
                # both 0 -> max deviceId
                if a["deviceId"] > b["deviceId"]:
                    winner = a
            else:
                # both >0 -> max updatedAt tie->max deviceId
                if (a["updatedAt"], a["deviceId"]) > (b["updatedAt"], b["deviceId"]):
                    winner = a
        # THEN if winner.updatedAt ==0 set to FIXED_STAMP
        if winner["updatedAt"] == 0:
            # copy to avoid mutating original candidate
            winner = dict(winner)
            winner["updatedAt"] = FIXED_STAMP
        winners.append(winner)
    winners.sort(key=lambda x: x["key"])
    return winners

def reference_merge_stats(all_items):
    """Union dedup by eventId, sort day then eventId. Keep first occurrence for duplicates (identical expected)."""
    seen = {}
    for it in all_items:
        eid = it["eventId"]
        if eid not in seen:
            seen[eid] = it
        else:
            # If duplicate with same eventId but different payload, keep first (no conflict expected)
            # We could keep max updatedAt if present, but reference says union no conflict, so keep first.
            pass
    winners = list(seen.values())
    winners.sort(key=lambda x: (x["day"], x["eventId"]))
    return winners

def gen_dict_op(device_id):
    spoken = random.choice(DICT_SPOKEN_VARIANTS)
    corrected = random.choice(DICT_CORRECTED_POOL)
    # Ensure some tombstones
    is_tomb = random.random() < 0.15
    updated_at = random.randint(1713465000000, 1713471000000) if random.random() > 0.05 else random.randint(100, 500)
    # ensure updatedAt >0 normally, but for dict we keep >0
    deleted_at = updated_at if is_tomb else None
    # occasionally create overlapping businessKey with different case
    # isEnabled random but distinct from tombstone
    is_enabled = random.choice([True, True, True, False])  # mostly true
    return {
        "kind": "dict",
        "op": "put",
        "syncId": det_uuid(),
        "spoken": spoken,
        "corrected": corrected,
        "isEnabled": is_enabled,
        "updatedAt": updated_at,
        "deviceId": device_id,
        "deletedAt": deleted_at,
    }

def gen_snip_op(device_id):
    trigger = random.choice(SNIPPET_TRIGGER_VARIANTS)
    expansion = random.choice(SNIPPET_EXPANSION_POOL)
    is_tomb = random.random() < 0.12
    updated_at = random.randint(1713468000000, 1713471000000)
    deleted_at = updated_at if is_tomb else None
    is_enabled = random.choice([True, True, False])
    return {
        "kind": "snip",
        "op": "put",
        "syncId": det_uuid(),
        "trigger": trigger,
        "expansion": expansion,
        "isEnabled": is_enabled,
        "updatedAt": updated_at,
        "deviceId": device_id,
        "deletedAt": deleted_at,
    }

def gen_set_op(device_id):
    key = random.choice(ALLOWED_SETTINGS)
    value = random.choice(SETTINGS_VALUES[key])
    # Include some updatedAt=0 adoption cases (approx 12%)
    if random.random() < 0.12:
        updated_at = 0
    else:
        # Use timestamp larger than current wall-clock (~1787...) so t>0 beats stamped t0 (1700... or wall-clock)
        updated_at = random.randint(1790000000000 - 100000, 1790000000000 + 5000)
    return {
        "kind": "set",
        "op": "put",
        "key": key,
        "value": value,
        "updatedAt": updated_at,
        "deviceId": device_id,
    }

def gen_stat_op(device_id):
    # device_id not in spec but we keep for trace; expected will not have it
    day = random.choice(DAYS_POOL)
    # timestampMs derived from day midnight + offset for realism
    try:
        dt = datetime.datetime.strptime(day, "%Y-%m-%d").replace(tzinfo=datetime.timezone.utc)
        base = int(dt.timestamp() * 1000)
    except:
        base = 1700000000000
    timestamp_ms = base + random.randint(0, 86399999)
    # Ensure within bounds
    words = random.randint(1, 500)
    chars = words * random.randint(3, 7)
    duration_ms = random.randint(1000, 120000)
    # Ensure caps
    if chars > 10_000_000:
        chars = 10_000_000
    return {
        "kind": "stat",
        "op": "put",
        "eventId": det_uuid(),
        "day": day,
        "timestampMs": timestamp_ms,
        "words": words,
        "chars": chars,
        "durationMs": duration_ms,
    }

def generate_scenario(idx):
    """Generate scenario idx (1..30). Returns dict with name, devices, expected, and ops_total."""
    seed = FIXED_SEEDS[idx-1]
    random.seed(seed)
    name = f"s{idx:02d}"
    # 2 or 3 devices; pattern deterministic
    num_devices = 2 if idx % 3 == 0 else 3
    device_ids = ["dev-a", "dev-b", "dev-c"][:num_devices]
    devices = {d: [] for d in device_ids}
    total_ops = random.randint(10, 40)
    # Adversarial injections count: sameKey(len) +2 +2+2+1+4 = len+11
    adversarial_slots = len(device_ids) + 11
    if total_ops < adversarial_slots + 3:
        total_ops = adversarial_slots + 3
    if total_ops > 40:
        total_ops = 40
    base_count = total_ops - adversarial_slots

    # Generate base random ops
    for _ in range(base_count):
        dev = random.choice(device_ids)
        kind_roll = random.random()
        if kind_roll < 0.38:
            op = gen_dict_op(dev)
        elif kind_roll < 0.60:
            op = gen_snip_op(dev)
        elif kind_roll < 0.80:
            op = gen_set_op(dev)
        else:
            op = gen_stat_op(dev)
        devices[dev].append(op)

    # --- Adversarial injections (deterministic per scenario) ---
    # 1) Same businessKey created on 2-3 devices with different syncIds, different timestamps
    spoken_same = random.choice(["hello", " Hello", "HELLO", "नमस्ते", "😀-test", "привет"])
    # Create on dev-a and dev-b (and dev-c if 3 devices)
    for dev in device_ids:
        op = {
            "kind": "dict",
            "op": "put",
            "syncId": det_uuid(),
            "spoken": spoken_same if dev == device_ids[0] else spoken_same.strip().upper() if dev == device_ids[1] else "  " + spoken_same + "  ",
            "corrected": random.choice(DICT_CORRECTED_POOL),
            "isEnabled": True,
            "updatedAt": 1713469000000 + random.randint(0, 10000) + (0 if dev == device_ids[0] else 500),
            "deviceId": dev,
            "deletedAt": None,
        }
        devices[dev].append(op)

    # 2) Delete-vs-edit same ts (tiebreak) — same businessKey, same updatedAt, different deviceId, one tombstone
    tie_spoken = "teh"
    tie_ts = 1713470000000 + (idx * 10)  # same ts for both
    # Ensure deviceId lexicographically max wins
    devs_for_tie = device_ids[:2]
    # tombstone with larger deviceId should win if tie
    # Create tombstone on dev with larger lexicographic id (dev-b > dev-a)
    tomb_dev = max(devs_for_tie)
    live_dev = min(devs_for_tie)
    tomb_op = {
        "kind": "dict",
        "op": "put",
        "syncId": det_uuid(),
        "spoken": tie_spoken,
        "corrected": "the",
        "isEnabled": True,
        "updatedAt": tie_ts,
        "deviceId": tomb_dev,
        "deletedAt": tie_ts,
    }
    live_op = {
        "kind": "dict",
        "op": "put",
        "syncId": det_uuid(),
        "spoken": tie_spoken.upper(),
        "corrected": "edited-live",
        "isEnabled": True,
        "updatedAt": tie_ts,
        "deviceId": live_dev,
        "deletedAt": None,
    }
    devices[tomb_dev].append(tomb_op)
    devices[live_dev].append(live_op)

    # 3) Delete-then-recreate: same businessKey, tombstone then live with newer ts
    recreate_spoken = "gonna"
    tomb_ts = 1713465000000 + idx * 100
    live_ts = tomb_ts + 5000  # newer
    # Tombstone on dev-a
    devices[device_ids[0]].append({
        "kind": "dict",
        "op": "put",
        "syncId": det_uuid(),
        "spoken": recreate_spoken,
        "corrected": "going to - old",
        "isEnabled": True,
        "updatedAt": tomb_ts,
        "deviceId": device_ids[0],
        "deletedAt": tomb_ts,
    })
    # Recreate live on another device
    rec_dev = device_ids[1] if len(device_ids) > 1 else device_ids[0]
    devices[rec_dev].append({
        "kind": "dict",
        "op": "put",
        "syncId": det_uuid(),
        "spoken": recreate_spoken,
        "corrected": "going to - recreated",
        "isEnabled": True,
        "updatedAt": live_ts,
        "deviceId": rec_dev,
        "deletedAt": None,
    })

    # 4) Duplicate identical stats events on 2 devices
    dup_event_id = det_uuid()
    dup_day = random.choice(DAYS_POOL)
    dup_ts = 1787184000000 + idx * 1000
    dup_words = random.randint(10, 100)
    dup_chars = dup_words * 5
    dup_dur = random.randint(5000, 60000)
    dup_stat = {
        "kind": "stat",
        "op": "put",
        "eventId": dup_event_id,
        "day": dup_day,
        "timestampMs": dup_ts,
        "words": dup_words,
        "chars": dup_chars,
        "durationMs": dup_dur,
    }
    # Add identical duplicate to two devices (if 2 devices, both; if 3, pick 2)
    for dev in device_ids[:2]:
        devices[dev].append(dict(dup_stat))

    # 5) Unicode + whitespace/case variants — already covered via spoken_same, but ensure at least one Hindi/emoji/Cyrillic
    # Add a snippet unicode trigger
    uni_trigger = random.choice(["नमस्ते", "привет", "😀-test"])
    devices[random.choice(device_ids)].append({
        "kind": "snip",
        "op": "put",
        "syncId": det_uuid(),
        "trigger": uni_trigger,
        "expansion": random.choice(SNIPPET_EXPANSION_POOL),
        "isEnabled": True,
        "updatedAt": 1713468000000 + random.randint(0, 5000),
        "deviceId": random.choice(device_ids),
        "deletedAt": None,
    })

    # 6) Settings t=0 adoption cases and t=0 vs t>0 conflicts
    # Create two settings ops for same key: one with t=0, one with t>0
    set_key = random.choice(ALLOWED_SETTINGS)
    # t=0 winner candidate
    devices[device_ids[0]].append({
        "kind": "set",
        "op": "put",
        "key": set_key,
        "value": random.choice(SETTINGS_VALUES[set_key]),
        "updatedAt": 0,
        "deviceId": device_ids[0],
    })
    # t>0 candidate for same key on other device (use large timestamp > wall-clock so t>0 beats stamped t0)
    other_dev = device_ids[1] if len(device_ids) > 1 else device_ids[0]
    devices[other_dev].append({
        "kind": "set",
        "op": "put",
        "key": set_key,
        "value": random.choice(SETTINGS_VALUES[set_key]),
        "updatedAt": 1790000000000 + random.randint(0, 1000),
        "deviceId": other_dev,
    })
    # Plus single t=0 adoption for another key (avoid both-0 same key which is order-dependent per-step stamping)
    set_key2 = random.choice([k for k in ALLOWED_SETTINGS if k != set_key])
    devices[device_ids[0]].append({
        "kind": "set",
        "op": "put",
        "key": set_key2,
        "value": random.choice(SETTINGS_VALUES[set_key2]),
        "updatedAt": 0,
        "deviceId": device_ids[0],
    })

    # Shuffle each device's ops to simulate realistic ordering (deterministic via random)
    for dev in devices:
        random.shuffle(devices[dev])

    # Compute expected via reference model over UNION of all final per-key/event states (LWW as above)
    # Collect all dict/snip/set/stat ops across devices
    all_dict = []
    all_snip = []
    all_set = []
    all_stat = []
    for dev, ops in devices.items():
        for op in ops:
            if op["kind"] == "dict":
                # Normalize to expected item shape for reference input
                all_dict.append({
                    "syncId": op["syncId"],
                    "spoken": op["spoken"],
                    "corrected": op["corrected"],
                    "isEnabled": op["isEnabled"],
                    "updatedAt": op["updatedAt"],
                    "deletedAt": op["deletedAt"],
                    "deviceId": op["deviceId"],
                })
            elif op["kind"] == "snip":
                all_snip.append({
                    "syncId": op["syncId"],
                    "trigger": op["trigger"],
                    "expansion": op["expansion"],
                    "isEnabled": op["isEnabled"],
                    "updatedAt": op["updatedAt"],
                    "deletedAt": op["deletedAt"],
                    "deviceId": op["deviceId"],
                })
            elif op["kind"] == "set":
                all_set.append({
                    "key": op["key"],
                    "value": op["value"],
                    "updatedAt": op["updatedAt"],
                    "deviceId": op["deviceId"],
                })
            elif op["kind"] == "stat":
                all_stat.append({
                    "eventId": op["eventId"],
                    "day": op["day"],
                    "timestampMs": op["timestampMs"],
                    "words": op["words"],
                    "chars": op["chars"],
                    "durationMs": op["durationMs"],
                })

    expected_dict = reference_merge_dictionary(all_dict)
    expected_snip = reference_merge_snippets(all_snip)
    expected_set = reference_merge_settings(all_set)
    expected_stat = reference_merge_stats(all_stat)

    # Build expected with keys in contract order, and ensure sorted already
    # For dictionary expected, keys order: syncId, businessKey, spoken, corrected, isEnabled, updatedAt, deletedAt, deviceId
    # We'll construct OrderedDict-like dicts with that insertion order
    exp_dict_ordered = []
    for it in expected_dict:
        exp_dict_ordered.append({
            "syncId": it["syncId"],
            "businessKey": it["businessKey"],
            "spoken": it["spoken"],
            "corrected": it["corrected"],
            "isEnabled": it["isEnabled"],
            "updatedAt": it["updatedAt"],
            "deletedAt": it["deletedAt"],
            "deviceId": it["deviceId"],
        })
    exp_snip_ordered = []
    for it in expected_snip:
        exp_snip_ordered.append({
            "syncId": it["syncId"],
            "businessKey": it["businessKey"],
            "trigger": it["trigger"],
            "expansion": it["expansion"],
            "isEnabled": it["isEnabled"],
            "updatedAt": it["updatedAt"],
            "deletedAt": it["deletedAt"],
            "deviceId": it["deviceId"],
        })
    exp_set_ordered = []
    for it in expected_set:
        d = {
            "key": it["key"],
            "value": it["value"],
            "updatedAt": it["updatedAt"],
            "deviceId": it["deviceId"],
        }
        # include deletedAt if present (not in our ops, but keep optional)
        if "deletedAt" in it and it["deletedAt"] is not None:
            d["deletedAt"] = it["deletedAt"]
        exp_set_ordered.append(d)
    exp_stat_ordered = []
    for it in expected_stat:
        exp_stat_ordered.append({
            "eventId": it["eventId"],
            "day": it["day"],
            "timestampMs": it["timestampMs"],
            "words": it["words"],
            "chars": it["chars"],
            "durationMs": it["durationMs"],
        })

    scenario = {
        "name": name,
        "devices": devices,
        "expected": {
            "dictionary": exp_dict_ordered,
            "snippets": exp_snip_ordered,
            "settings": exp_set_ordered,
            "stats": exp_stat_ordered,
        }
    }
    ops_total = sum(len(v) for v in devices.values())
    return scenario, ops_total

def main():
    WINDOWS_CONV.mkdir(parents=True, exist_ok=True)
    ANDROID_CONV.mkdir(parents=True, exist_ok=True)
    total_ops = 0
    for idx in range(1, 31):
        scenario, ops = generate_scenario(idx)
        total_ops += ops
        # Pretty-printed JSON, ensure_ascii=False to preserve unicode, indent=2
        fname = f"scenario-{idx:02d}.json"
        wpath = WINDOWS_CONV / fname
        apath = ANDROID_CONV / fname
        # Write with same bytes to both
        content = json.dumps(scenario, indent=2, ensure_ascii=False, sort_keys=False)
        # Ensure trailing newline
        if not content.endswith("\n"):
            content += "\n"
        wpath.write_text(content, encoding="utf-8")
        apath.write_text(content, encoding="utf-8")
        print(f"{fname}: ops={ops} name={scenario['name']}")
    print(f"Generated 30 scenarios, total ops={total_ops}")
    # also report fixed seeds
    print(f"Fixed seeds: {FIXED_SEEDS[:3]} ... {FIXED_SEEDS[-3:]}")

if __name__ == "__main__":
    main()
