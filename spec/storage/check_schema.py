#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Apply the storage migrations to a fresh in-memory SQLite DB and exercise
the invariants that the schema is meant to enforce (spec/storage/*.sql).
Exit code 0 when every check behaves as specified."""
import pathlib
import sqlite3
import sys

HERE = pathlib.Path(__file__).parent
NOW = 1_761_700_000_000

checks = []


def expect(name, fn, should_fail):
    try:
        fn()
        ok = not should_fail
        detail = "accepted"
    except sqlite3.DatabaseError as e:
        ok = should_fail
        detail = f"rejected: {e}"
    checks.append((ok, name, detail))


def main():
    db = sqlite3.connect(":memory:")
    db.execute("PRAGMA foreign_keys = ON")
    for mig in sorted(HERE.glob("[0-9][0-9][0-9][0-9]_*.sql")):
        db.executescript(mig.read_text())
    x = db.execute

    x("INSERT INTO ports VALUES ('p1','radio_serial','IC-9700',1,'icom-ic9700','usb:ABC','{\"baud\":9600}','slow',?,?)", (NOW, NOW))
    x("INSERT INTO ports VALUES ('p2','ratflector','sewx',1,NULL,NULL,'{\"host\":\"sewx.ratflector.com\"}','slow',?,?)", (NOW, NOW))
    x("INSERT INTO stations VALUES ('W1AW',?,?,NULL,NULL,NULL,NULL)", (NOW, NOW))

    expect("radio port requires a hardware profile",
           lambda: x("INSERT INTO ports VALUES ('p3','radio_serial','X',1,NULL,NULL,'{}','slow',?,?)", (NOW, NOW)), True)
    expect("ratflector port must not carry a radio profile",
           lambda: x("INSERT INTO ports VALUES ('p4','ratflector','Y',1,'icom-ic705',NULL,'{}','slow',?,?)", (NOW, NOW)), True)
    expect("callsign longer than 8 chars rejected (DDT2 field is 8 bytes)",
           lambda: x("INSERT INTO station_identity VALUES ('i1','N0CALL-123','N0CALL-123',1,?)", (NOW,)), True)
    expect("invalid JSON config rejected",
           lambda: x("INSERT INTO ports VALUES ('p5','sim','sim',1,NULL,NULL,'not json','slow',?,?)", (NOW, NOW)), True)
    expect("unknown fd_state rejected",
           lambda: x("INSERT INTO peer_capabilities VALUES ('W1AW','p1','aether','1.0','maybe',NULL,NULL,NULL,?)", (NOW,)), True)

    def add_msg(mid, src, dst, state):
        x("INSERT INTO messages(id,mid,folder,src,dst,xml,state,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
          (mid, mid, 'outbox', src, dst, '<form/>', state, NOW, NOW))

    expect("self-addressed queued message refused (P17)", lambda: add_msg("m0", "N0CALL", "N0CALL", "queued"), True)
    add_msg("m1", "N0CALL", "W1AW", "queued")
    expect("delivered without end-to-end evidence refused (SM-7)",
           lambda: x("UPDATE messages SET state='delivered' WHERE id='m1'"), True)
    x("INSERT INTO message_events(message_id,at,state,hop,evidence) VALUES ('m1',?,'handed_off','W1AW','arq_ack')", (NOW,))
    expect("hop-level ARQ ack still insufficient for delivered",
           lambda: x("UPDATE messages SET state='delivered' WHERE id='m1'"), True)
    expect("unconfirmed allowed with hop evidence",
           lambda: x("UPDATE messages SET state='unconfirmed' WHERE id='m1'"), False)
    add_msg("m2", "N0CALL", "K7ABC", "sending")
    x("INSERT INTO message_events(message_id,at,state,evidence) VALUES ('m2',?,'delivered','alp_complete')", (NOW,))
    expect("delivered allowed with alp_complete evidence",
           lambda: x("UPDATE messages SET state='delivered' WHERE id='m2'"), False)
    expect("message_events is append-only (update)",
           lambda: x("UPDATE message_events SET state='x'"), True)
    expect("message_events is append-only (delete)",
           lambda: x("DELETE FROM message_events"), True)
    expect("message with journal cannot be deleted",
           lambda: x("DELETE FROM messages WHERE id='m1'"), True)
    expect("outgoing chat needs tx_state; 'delivered' is not a chat state",
           lambda: x("INSERT INTO chat_lines(port_id,direction,src,dst,kind,body,at,tx_state) VALUES ('p1','out','N0CALL','CQCQCQ','text','hi',?,'delivered')", (NOW,)), True)
    expect("incoming chat must not have tx_state",
           lambda: x("INSERT INTO chat_lines(port_id,direction,src,dst,kind,body,at,tx_state) VALUES ('p1','in','W1AW','CQCQCQ','text','hi',?,'queued')", (NOW,)), True)
    expect("valid chat line accepted",
           lambda: x("INSERT INTO chat_lines(port_id,direction,src,dst,kind,body,at,tx_state) VALUES ('p1','out','N0CALL','CQCQCQ','text','hi',?,'transmitted')", (NOW,)), False)
    expect("latitude out of range rejected",
           lambda: x("INSERT INTO positions(callsign,received_at,lat,lon,source_fmt) VALUES ('W1AW',?,91,0,'nmea')", (NOW,)), True)
    expect("STRICT typing: text in integer column rejected",
           lambda: x("INSERT INTO transfers(id,direction,protocol,peer,filename,total_bytes,block_size,state,created_at,updated_at) VALUES ('t1','send','legacy_file','W1AW','f','big',512,'queued',?,?)", (NOW, NOW)), True)
    expect("valid legacy transfer accepted",
           lambda: x("INSERT INTO transfers(id,direction,protocol,peer,filename,total_bytes,block_size,state,wrap_warning,created_at,updated_at) VALUES ('t2','send','legacy_file','W1AW','f',300000,512,'queued',1,?,?)", (NOW, NOW)), False)
    v = x("SELECT max(version) FROM schema_migrations").fetchone()[0]
    checks.append((v == 1, "schema_migrations records version 1", f"version={v}"))

    failed = 0
    for ok, name, detail in checks:
        print(("PASS " if ok else "FAIL ") + name + "  [" + detail + "]")
        failed += not ok
    print(f"\n{len(checks) - failed}/{len(checks)} checks passed (SQLite {sqlite3.sqlite_version})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
