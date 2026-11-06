import type { SessionResponse, Workspace } from "./types";

/** One session of a shared worktree as its own workspace, so its triage overlay, menu and selection never touch the group's. */
export function sessionWorkspace(group: Workspace, session: SessionResponse): Workspace {
  return { ...group, id: `${group.id}::session::${session.id}`, sessions: [session] };
}

/** Every expandable group's per-session workspaces, for reconciling their optimistic overrides. */
export function sessionWorkspaces(groups: readonly Workspace[]): Workspace[] {
  return groups.flatMap((g) => (g.sessions.length > 1 ? g.sessions.map((s) => sessionWorkspace(g, s)) : []));
}
