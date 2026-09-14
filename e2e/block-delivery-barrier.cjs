const { execFileSync } = require("node:child_process");
const { isAbsolute } = require("node:path");

/** Read one exact fixture story and its durable effects in a single snapshot.
 * mode=ro must not create a store when isolation is missing or misspelled. */
function readBlockDeliverySnapshot(storePath, project, story) {
  if (!storePath || !isAbsolute(storePath)) throw new Error("cleanup requires an absolute isolated STORYHOOK_STORE_PATH");
  return JSON.parse(execFileSync("python3", ["-c", `
import json, pathlib, sqlite3, sys
path, project, story = sys.argv[1:]
with sqlite3.connect(pathlib.Path(path).as_uri() + "?mode=ro", uri=True) as db:
    db.execute("BEGIN")
    identities = db.execute("""
        SELECT p.id,p.uuid,p.slug,p.prefix,p.checkout_path,s.story_no,s.created_at
        FROM projects p JOIN stories s ON s.project_id=p.id
        WHERE p.slug=? AND p.prefix || '-' || s.story_no=?
    """, (project, story)).fetchall()
    if len(identities) != 1:
        raise RuntimeError("cleanup story identity is absent or ambiguous: " + project + "/" + story)
    identity = identities[0]
    deliveries = db.execute("""
        SELECT id,action,status FROM block_deliveries
        WHERE project_id=? AND story_no=? ORDER BY id
    """, (identity[0], identity[5])).fetchall()
    print(json.dumps({"identity": identity, "deliveries": deliveries}))
`, storePath, project, story], { encoding: "utf8", timeout: 5_000, stdio: ["ignore", "pipe", "pipe"] }));
}

/** Require explicit terminal acknowledgement; disappearing rows are not success.
 * Failures latch because expect.poll may evaluate again after an exception. */
class BlockDeliveryBarrier {
  /** Pin the canonical project/story whose deletion the caller is preparing. */
  constructor(project, story) {
    this.project = project;
    this.story = story;
    this.identity = null;
    this.seen = new Map();
    this.failure = null;
  }

  /** Return the outstanding IDs/statuses, or refuse lost or changed evidence. */
  observe(snapshot) {
    if (this.failure) throw this.failure;
    try {
      const identity = snapshot.identity;
      if (!Array.isArray(identity) || identity.length !== 7 || identity[2] !== this.project || `${identity[3]}-${identity[5]}` !== this.story) {
        throw new Error("cleanup snapshot names a different project/story");
      }
      const encoded = JSON.stringify(identity);
      if (this.identity !== null && this.identity !== encoded) throw new Error("cleanup story identity changed while waiting for delivery");
      this.identity = encoded;
      const current = new Map();
      const outstanding = [];
      for (const [id, action, status] of snapshot.deliveries) {
        if (!Number.isSafeInteger(id) || id <= 0 || current.has(id) || !["interrupt", "resume"].includes(action)) throw new Error("invalid cleanup delivery identity");
        if (this.seen.has(id) && this.seen.get(id) !== action) throw new Error(`cleanup delivery ${id} changed action`);
        if (["pending", "attempting"].includes(status)) outstanding.push(`${id}:${status}`);
        else if (!["delivered", "unreached", "uncertain", "superseded"].includes(status)) throw new Error(`unknown delivery status: ${status}`);
        current.set(id, action);
      }
      for (const id of this.seen.keys()) {
        if (!current.has(id)) throw new Error(`cleanup delivery ${id} disappeared before the completion barrier`);
      }
      this.seen = current;
      return outstanding;
    } catch (error) {
      this.failure = error;
      throw error;
    }
  }
}

module.exports = { BlockDeliveryBarrier, readBlockDeliverySnapshot };
