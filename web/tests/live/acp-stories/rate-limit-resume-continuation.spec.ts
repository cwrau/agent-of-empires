// User story: a structured view turn is interrupted by a provider rate limit
// and parks the session. When the user clicks "Resume now", aoe must not only
// respawn the worker but also re-issue the interrupted prompt so the agent
// continues instead of sitting idle. See #3028.
//
// The fake ACP agent rate-limits the first turn, then (via a persisted turn
// cursor that survives the resume respawn) answers the next prompt normally
// with a distinct marker. Asserting that marker appears in the replay proves
// the interrupted prompt was automatically re-sent on resume.

import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test as base, expect } from "@playwright/test";
import { spawnAoeServe, listSessions, seedSessionViaAoeAdd } from "../../helpers/aoeServe";
import {
  enableStructuredViewAndWait,
  waitForStructuredView,
  waitForReplayContains,
  attachServeDiagnostics,
} from "../../helpers/acp";

const RESETS_AT = new Date(Date.now() + 60 * 60 * 1000).toISOString();

// Turn 0 rate-limits; turn 1 (served to the resumed worker) is the
// continuation and carries a marker distinct from turn 0.
const SCRIPT = {
  turns: [
    {
      updates: [{ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Starting the task." } }],
      rateLimit: { resets_at: RESETS_AT, message: "usage limit reached" },
    },
    {
      updates: [
        { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Resumed and continued the task." } },
      ],
      stopReason: "end_turn",
    },
  ],
};

base("resume re-issues the interrupted prompt so the agent continues", async ({ page }, testInfo) => {
  let serveHandle: { home: string } | undefined;
  let serve: Awaited<ReturnType<typeof spawnAoeServe>> | undefined;
  const scriptDir = mkdtempSync(join(tmpdir(), "aoe-pw-rl-resume-"));
  const scriptPath = join(scriptDir, "script.json");
  const turnStatePath = join(scriptDir, "turn-cursor");
  writeFileSync(scriptPath, JSON.stringify(SCRIPT));

  try {
    serve = await spawnAoeServe({
      authMode: "none",
      acp: true,
      fakeAcpScript: scriptPath,
      // Persist the fake agent's turn cursor across the resume respawn so the
      // continuation prompt gets turn 1, not turn 0 again.
      extraEnv: { FAKE_ACP_TURN_STATE: turnStatePath },
      workerIndex: testInfo.workerIndex,
      parallelIndex: testInfo.parallelIndex,
      seedFn: seedSessionViaAoeAdd({ title: "rl-resume" }),
    });
    serveHandle = serve;

    const sessions = await listSessions(serve.baseUrl);
    const session = sessions.find((s) => s.title === "rl-resume");
    if (!session) throw new Error("seeded session 'rl-resume' missing");

    await enableStructuredViewAndWait(serve.baseUrl, session.id, 30_000, serve.home);

    await page.goto(`${serve.baseUrl}/session/${encodeURIComponent(session.id)}`);
    await waitForStructuredView(page);

    const composer = page.getByRole("textbox", { name: /Send a message|Queue a follow-up/i });
    await composer.fill("keep working on the task");
    await composer.press("Enter");

    // The turn parks on the rate limit.
    await expect(page.getByText(/Rate-limited/i)).toBeVisible({ timeout: 15_000 });

    // Resume: respawns the worker AND (the #3028 fix) re-issues the
    // interrupted prompt via the pending-initial-turn drain.
    await page.getByRole("button", { name: /Resume now/i }).click();

    // The continuation marker only appears if the interrupted prompt was
    // re-sent to the resumed worker.
    await waitForReplayContains(serve.baseUrl, session.id, "Resumed and continued the task.", {
      timeoutMs: 30_000,
    });
  } finally {
    try {
      if (serveHandle) await attachServeDiagnostics(testInfo, serveHandle);
    } catch {
      // best-effort diagnostics; do not block cleanup
    }
    try {
      if (serve) await serve.stop();
    } finally {
      rmSync(scriptDir, { recursive: true, force: true });
    }
  }
});
