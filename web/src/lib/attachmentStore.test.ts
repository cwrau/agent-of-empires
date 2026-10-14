// @vitest-environment jsdom

import { describe, expect, it } from "vitest";

import { deleteAttachments, loadAttachments, saveAttachments } from "./attachmentStore";

describe("attachmentStore", () => {
  const att = (dataB64: string) => [{ kind: "image" as const, mimeType: "image/png", dataB64 }];

  it("a delete only removes a record that no newer revision replaced", async () => {
    await saveAttachments("s-rev", att("OLD"), [100, "a"]);
    await saveAttachments("s-rev", att("NEWER"), [200, "a"]);
    await deleteAttachments("s-rev", [150, "a"]);
    expect(await loadAttachments("s-rev")).toEqual(att("NEWER"));
    await deleteAttachments("s-rev", [200, "a"]);
    expect(await loadAttachments("s-rev")).toBeUndefined();
    await saveAttachments("s-rev", att("ANY"), [300, "a"]);
    await deleteAttachments("s-rev");
    expect(await loadAttachments("s-rev")).toBeUndefined();
  });

  it("breaks equal clocks by tab id, so exactly one side is newer", async () => {
    await saveAttachments("s-tie", att("FROM-B"), [100, "b"]);
    await deleteAttachments("s-tie", [100, "a"]);
    expect(await loadAttachments("s-tie")).toEqual(att("FROM-B"));
    await deleteAttachments("s-tie", [100, "b"]);
    expect(await loadAttachments("s-tie")).toBeUndefined();
    await saveAttachments("s-tie", att("FROM-A"), [100, "a"]);
    await saveAttachments("s-tie", att("FROM-B"), [100, "b"]);
    await saveAttachments("s-tie", att("LATE-A"), [100, "a"]);
    expect(await loadAttachments("s-tie")).toEqual(att("FROM-B"));
  });

  it("a queued older save cannot overwrite a newer one that landed first", async () => {
    await saveAttachments("s-a2", att("A1"), [100, "a"]);
    await saveAttachments("s-a2", att("B"), [300, "b"]);
    await saveAttachments("s-a2", att("A2"), [200, "a"]);
    expect(await loadAttachments("s-a2")).toEqual(att("B"));
  });

  it("reopens after another tab deletes the database", async () => {
    await saveAttachments("s-1", [{ kind: "image", mimeType: "image/png", dataB64: "AAAA" }]);
    // deleteDatabase fires versionchange on our open connection, which closes it.
    await new Promise<void>((resolve, reject) => {
      const req = indexedDB.deleteDatabase("aoe-composer-attachments");
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
    await saveAttachments("s-2", [{ kind: "image", mimeType: "image/png", dataB64: "BBBB" }]);
    expect(await loadAttachments("s-1")).toBeUndefined();
    expect(await loadAttachments("s-2")).toEqual([{ kind: "image", mimeType: "image/png", dataB64: "BBBB" }]);
  });
});
