/**
 * `boardStore.reviewTask` — creation-approval optimistic update.
 *
 * The PM server has TWO review semantics keyed on `status` (see
 * `core/acowork-pm/src/store/tree.rs::review_task`):
 *
 *   - `submitted` — an Agent submitted its work; approve → `done`.
 *   - `pending` + `review_status=pending` — an Agent created the task and
 *     it awaits human approval to start; approve flips `review_status`
 *     only and the task STAYS `pending` (the Agent then claims it).
 *
 * The optimistic update has to mirror that split. If it blanket-wrote
 * `status: "done"` on approve, a freshly-approved creation-approval card
 * would flash into the Done column and then bounce back to Pending when
 * the server response landed — a visible jump on every approval click.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import type { PmTaskResponse } from "../../lib/pm-types";

const mockReviewTask =
  vi.fn<(tid: string, approved: boolean, comment?: string) => Promise<PmTaskResponse>>();

vi.mock("../../lib/pm-api", () => ({
  reviewTask: (tid: string, approved: boolean, comment?: string) =>
    mockReviewTask(tid, approved, comment),
  listProjectTasks: () => Promise.resolve([]),
}));
vi.mock("../../components/common/ToastProvider", () => ({ showToast: () => {} }));
vi.mock("../../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));

const { usePmBoardStore } = await import("./boardStore");

function task(over: Partial<PmTaskResponse>): PmTaskResponse {
  return {
    id: "t-1",
    project_id: "p-1",
    title: "T",
    description: "",
    type: "task",
    status: "pending",
    review_status: "not_required",
    priority: "normal",
    assignee: null,
    due_at: null,
    depends_on: [],
    attachments: [],
    result: null,
    created_by: "agent-x",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    claimed_at: null,
    submitted_at: null,
    is_blocked: false,
    blocked_by: [],
    depth: 0,
    parent_id: null,
    ...over,
  };
}

beforeEach(() => {
  usePmBoardStore.getState().clear();
  mockReviewTask.mockReset();
});

describe("reviewTask optimistic update", () => {
  it("approve on a creation-approval (pending) keeps status=pending", async () => {
    const t = task({ id: "t-a", status: "pending", review_status: "pending" });
    usePmBoardStore.setState({ tasks: [t] });

    // Capture the state the card sees *during* the request (before it resolves)
    let duringRequest: PmTaskResponse | undefined;
    mockReviewTask.mockImplementation(async () => {
      duringRequest = usePmBoardStore.getState().tasks[0];
      return { ...t, review_status: "approved" };
    });

    await usePmBoardStore.getState().reviewTask("t-a", true);

    expect(duringRequest?.status, "must not flash to done").toBe("pending");
    expect(duringRequest?.review_status).toBe("approved");
    const after = usePmBoardStore.getState().tasks[0];
    expect(after.status).toBe("pending");
    expect(after.review_status).toBe("approved");
  });

  it("approve on a submitted task still moves it to done", async () => {
    const t = task({ id: "t-b", status: "submitted", review_status: "not_required" });
    usePmBoardStore.setState({ tasks: [t] });
    mockReviewTask.mockResolvedValue({ ...t, status: "done", review_status: "approved" });

    await usePmBoardStore.getState().reviewTask("t-b", true);

    const after = usePmBoardStore.getState().tasks[0];
    expect(after.status).toBe("done");
  });

  it("reject moves both shapes to rejected", async () => {
    const t = task({ id: "t-c", status: "pending", review_status: "pending" });
    usePmBoardStore.setState({ tasks: [t] });
    mockReviewTask.mockResolvedValue({ ...t, status: "rejected", review_status: "rejected" });

    await usePmBoardStore.getState().reviewTask("t-c", false, "垃圾任务");

    const after = usePmBoardStore.getState().tasks[0];
    expect(after.status).toBe("rejected");
    expect(after.review_status).toBe("rejected");
  });
});
