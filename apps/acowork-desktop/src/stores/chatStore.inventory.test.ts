/**
 * Inventory-change fanout regression (the bug that motivated this
 * signal — see `acowork/desktop/inventory` design).
 *
 * The Rust eventloop emits a Tauri `inventory-changed` event on every
 * `acowork/desktop/inventory` signal the Gateway publishes (install /
 * uninstall / retained inventory replay). The listener in `chatStore.ts`
 * bumps `inventoryVersion`; the AgentList sidebar subscribes to it and
 * refetches `GET /api/agents`.
 *
 * The signal is a **live, non-retained** message, so it cannot cover a
 * change that happened while the Desktop was disconnected. That catch-up
 * is the second bump source: every MQTT transition into `connected`
 * (initial subscribe + each reconnect) also bumps `inventoryVersion`.
 *
 * This file pins:
 *  1. `initMqttListener` registers the `inventory-changed` channel
 *     (so the Rust `app.emit` actually finds a subscriber).
 *  2. Each emitted `inventory-changed` bumps `inventoryVersion`.
 *  3. A (re)connect edge bumps `inventoryVersion` — without this, an
 *     install that lands while the Desktop is offline is never seen.
 *  4. `disposeMqttListener` cleans the listener up so a remount does
 *     not double-count events.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

type Handler = (event: { payload: unknown }) => void;

const { mockInvoke, mockListen, registeredListeners } = vi.hoisted(() => {
  const registeredListeners = new Map<string, Handler[]>();
  const mockInvoke = vi.fn(
    async (..._args: unknown[]): Promise<unknown> => undefined,
  );
  const mockListen = vi.fn(async (channel: string, handler: Handler) => {
    const arr = registeredListeners.get(channel) ?? [];
    arr.push(handler);
    registeredListeners.set(channel, arr);
    return () => {
      registeredListeners.set(
        channel,
        (registeredListeners.get(channel) ?? []).filter((h) => h !== handler),
      );
    };
  });
  return { mockInvoke, mockListen, registeredListeners };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

import {
  disposeMqttListener,
  initMqttListener,
  useChatStore,
} from "./chatStore";

function emit(channel: string, payload: unknown = {}): void {
  for (const handler of registeredListeners.get(channel) ?? []) {
    handler({ payload });
  }
}

const emitInventoryChanged = () => emit("inventory-changed", { ts_ms: 0 });

const inventoryVersion = () => useChatStore.getState().inventoryVersion;

beforeEach(() => {
  disposeMqttListener();
  registeredListeners.clear();
  mockInvoke.mockReset();
  mockListen.mockClear();
  // Default: get_mqtt_status returns a "known + connected" snapshot
  // so the chatStore init path doesn't take the snapshot-error branch
  // (we only care about the inventory fanout wiring here).
  mockInvoke.mockImplementation(async (cmd: unknown) => {
    if (cmd === "get_mqtt_status") {
      return { known: true, connected: true };
    }
    return undefined;
  });
  useChatStore.setState({ inventoryVersion: 0 });
});

afterEach(() => {
  disposeMqttListener();
});

describe("inventory-change fanout", () => {
  it("registers a listener on the `inventory-changed` Tauri channel", async () => {
    await initMqttListener();
    expect(
      registeredListeners.get("inventory-changed")?.length ?? 0,
    ).toBeGreaterThanOrEqual(1);
  });

  it("bumps `inventoryVersion` on each emitted `inventory-changed` event", async () => {
    await initMqttListener();
    const base = inventoryVersion();

    emitInventoryChanged();
    expect(inventoryVersion()).toBe(base + 1);

    emitInventoryChanged();
    expect(inventoryVersion()).toBe(base + 2);

    emitInventoryChanged();
    expect(inventoryVersion()).toBe(base + 3);
  });

  it("bumps on the initial connect edge (closes the mount-fetch vs subscribe window)", async () => {
    // `get_mqtt_status` reports connected, so init applies an
    // idle → connected transition: the sidebar must refetch once the
    // subscription is actually live.
    await initMqttListener();
    expect(inventoryVersion()).toBeGreaterThanOrEqual(1);
  });

  it("bumps on every reconnect edge (catch-up for changes missed while offline)", async () => {
    await initMqttListener();
    const afterInitialConnect = inventoryVersion();

    // Drop.
    emit("mqtt-status", { connected: false, reconnecting: true });
    // Rise. The Gateway may have mutated `installed_agents` while we
    // were away; the live signal for that change is long gone, so the
    // connect edge is the only trigger.
    emit("mqtt-status", { connected: true });

    expect(inventoryVersion()).toBeGreaterThan(afterInitialConnect);
  });

  it("does not double-count while already connected", async () => {
    await initMqttListener();
    const connected = inventoryVersion();

    // Re-delivered transient events / a second snapshot apply must not
    // churn the sidebar into repeated refetches.
    emit("mqtt-status", { connected: true });
    emit("mqtt-status", { connected: true });

    expect(inventoryVersion()).toBe(connected);
  });

  it("a burst of signal deliveries bumps once each", async () => {
    await initMqttListener();
    const base = inventoryVersion();

    emitInventoryChanged();
    emitInventoryChanged();
    emitInventoryChanged();

    expect(inventoryVersion()).toBe(base + 3);
  });

  it("dispose removes the listener so subsequent emits do NOT bump the counter", async () => {
    await initMqttListener();
    const beforeBump = inventoryVersion();
    emitInventoryChanged();
    expect(inventoryVersion()).toBe(beforeBump + 1);

    disposeMqttListener();
    emitInventoryChanged();
    // The listener was unlistened, so the bump does NOT happen.
    expect(inventoryVersion()).toBe(beforeBump + 1);
  });

  it("a fresh init after dispose re-registers and resumes bumping", async () => {
    await initMqttListener();
    disposeMqttListener();
    // Simulate StrictMode dev double-mount: dispose + reinit.
    await initMqttListener();

    expect(
      registeredListeners.get("inventory-changed")?.length ?? 0,
    ).toBeGreaterThanOrEqual(1);

    const before = inventoryVersion();
    emitInventoryChanged();
    expect(inventoryVersion()).toBe(before + 1);
  });
});
