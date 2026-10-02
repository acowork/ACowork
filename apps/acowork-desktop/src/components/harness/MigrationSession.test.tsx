//! The migration panel must outlive the tab that started it.
//!
//! The report: start a rebuild in Harness -> Embedding Models, visit the chat,
//! come back - the progress list is gone while the Gateway is still working on
//! it. The session lived in `useState` inside EmbeddingModelTab, so unmounting
//! the tab took the whole panel with it; the poll interval kept running but its
//! callback set state on an unmounted component, so nothing was ever
//! repainted. The job was invisible and, from the UI, unstoppable.
//!
//! The session now lives in the gateway store and polling is an effect keyed on
//! it, so a remount shows the same list and resumes the poll.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import i18n from "../../i18n";
import { useGatewayStore, type MigrationSession } from "../../stores/gatewayStore";
import { EmbeddingModelTab } from "./EmbeddingModelTab";
import { fetchMigrationProgress } from "../../lib/gateway-api";

vi.mock("../../lib/gateway-api", () => ({
  fetchEmbeddingModels: vi.fn().mockResolvedValue({
    models: [], active_model_id: null, service_running: true, embed_port: 18080, models_detail: []
  }),
  fetchCloudEmbeddingProviders: vi.fn().mockResolvedValue({ providers: [] }),
  fetchMigrationProgress: vi.fn().mockResolvedValue({ agents: [] }),
  startMigration: vi.fn().mockResolvedValue({ status: "migration_started" }),
  selectEmbeddingModelWithMigration: vi.fn(),
  selectEmbeddingModel: vi.fn(),
  testEmbeddingModel: vi.fn(),
  downloadEmbeddingModel: vi.fn(),
  fetchEmbeddingModelStatus: vi.fn().mockResolvedValue({ models: [] }),
  deleteEmbeddingModel: vi.fn(),
  selectCloudEmbeddingModel: vi.fn(),
  setCloudEmbeddingApiKey: vi.fn(),
  deleteCloudEmbeddingApiKey: vi.fn(),
  testCloudEmbeddingProvider: vi.fn(),
  addCloudEmbeddingProvider: vi.fn(),
}));

const AGENT_A = "aaaaaaaa-0000-0000-0000-000000000000";
const AGENT_B = "bbbbbbbb-0000-0000-0000-000000000000";

function makeSession(overrides: Partial<MigrationSession> = {}): MigrationSession {
  return {
    modelId: "bge-m3",
    oldDimension: 512,
    newDimension: 1024,
    message: "Dimension changed",
    agents: [
      { instance_id: AGENT_A, name: "com.acowork.ponytail", is_running: true, has_active_sessions: false },
      { instance_id: AGENT_B, name: "com.acowork.senior-engineer", is_running: true, has_active_sessions: false },
    ],
    selected: new Set([AGENT_A, AGENT_B]),
    started: true,
    ...overrides,
  };
}

/** Render then tear the tree down. The tab unmounts whenever the user
 *  navigates to the chat, and that unmount is the entire bug. */
function renderAndUnmount() {
  render(<EmbeddingModelTab />).unmount();
}

describe("migration session survives a tab switch", () => {
  beforeEach(() => {
    // The tab refuses to render anything without a Gateway, so the store has
    // to look connected for any of this to be on screen.
    useGatewayStore.setState({
      status: "connected",
      migrationSession: null,
      migrationProgress: {},
    });
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
    useGatewayStore.setState({ status: "disconnected" });
  });

  it("keeps the session in the store when the tab goes away", () => {
    useGatewayStore.setState({ migrationSession: makeSession() });

    renderAndUnmount();

    // The point of the fix: a dead component must not discard the session.
    expect(useGatewayStore.getState().migrationSession).not.toBeNull();
  });

  it("still lists the agents and their progress after unmount + remount", () => {
    useGatewayStore.setState({
      migrationSession: makeSession(),
      migrationProgress: {
        [AGENT_A]: {
          instance_id: AGENT_A,
          request_id: "r1",
          target_model_id: "bge-m3",
          target_dimension: 1024,
          progress: { rebuilt: 36, total_scanned: 1024, errors: 0, phase: "reembed", label: "" },
          done: false,
        },
      },
    });

    renderAndUnmount();
    render(<EmbeddingModelTab />);

    expect(screen.getByText("com.acowork.ponytail")).toBeTruthy();
    expect(screen.getByText("36/1024")).toBeTruthy();
    // Still running, so the panel says so rather than offering a fresh start.
    expect(screen.getByText(i18n.t("embedding.migrationInProgress"))).toBeTruthy();
  });

  it("resumes polling after remount, so a finished rebuild settles", async () => {
    vi.useFakeTimers();
    useGatewayStore.setState({ migrationSession: makeSession() });
    vi.mocked(fetchMigrationProgress).mockResolvedValue({
      agents: [
        {
          instance_id: AGENT_A,
          request_id: "r1",
          target_model_id: "bge-m3",
          target_dimension: 1024,
          progress: { rebuilt: 1024, total_scanned: 1024, errors: 0, phase: "reembed", label: "" },
          done: true,
        },
      ],
    });

    renderAndUnmount();
    render(<EmbeddingModelTab />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2500);
    });

    // The resumed poll reached the Gateway and wrote progress back into the
    // store - a remount did not strand the UI on stale numbers.
    expect(fetchMigrationProgress).toHaveBeenCalled();
    expect(useGatewayStore.getState().migrationProgress[AGENT_A]?.done).toBe(true);
  });

  it("pre-flight offers Start, and Cancel clears the session", () => {
    useGatewayStore.setState({ migrationSession: makeSession({ started: false }) });

    render(<EmbeddingModelTab />);

    expect(screen.getByText(i18n.t("embedding.migrationRequired"))).toBeTruthy();
    const start = screen.getByRole("button", { name: i18n.t("embedding.startMigration") });
    expect((start as HTMLButtonElement).disabled).toBe(false);

    act(() => {
      screen.getByRole("button", { name: "Cancel" }).click();
    });
    expect(useGatewayStore.getState().migrationSession).toBeNull();
  });

  it("will not start with nothing selected", () => {
    useGatewayStore.setState({ migrationSession: makeSession({ started: false, selected: new Set() }) });

    render(<EmbeddingModelTab />);

    const start = screen.getByRole("button", { name: i18n.t("embedding.startMigration") });
    expect((start as HTMLButtonElement).disabled).toBe(true);
  });
});
