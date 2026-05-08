// WebSocket client for ichatpp (TODO [41]).
//
// Singleton: one socket per browser tab, multiplexed across components.
// Auth: the browser ships the httpOnly `access_token` cookie during the
// upgrade — there is no in-band token to attach.
//
// Reconnect strategy: exponential back-off, capped at 30 s, per
// ARCHITECTURE.md §4.7. We reset the back-off on a successful open so a
// long quiet connection that briefly drops doesn't carry a 30-s delay
// into the next session.
//
// Dispatch: a tiny EventEmitter — components subscribe via the React
// hook below. We deliver every payload to every listener; per-event
// filtering is the listener's responsibility because most consumers
// already branch on `type` themselves.

export type ClientEvent =
  | { type: 'message'; content: string; emoji_ids: string[]; to_user_id: string }
  | { type: 'typing'; to_user_id: string }
  | { type: 'read'; message_id: string };

export type ServerEvent =
  | { type: 'message_received'; message_id: string; timestamp: string }
  | { type: 'user_online'; user_id: string }
  | { type: 'user_offline'; user_id: string }
  | {
      type: 'message';
      message_id: string;
      from_user_id: string;
      content: string;
      emoji_ids: string[];
      created_at: string;
    }
  | { type: 'error'; code: string; message: string };

export type WsStatus = 'idle' | 'connecting' | 'open' | 'closed';

export function getWebSocketUrl(): string {
  return process.env.NEXT_PUBLIC_WS_URL ?? 'ws://localhost:8080/api/ws';
}

// ────────────────────────────────────────────────────────────────────────
// Singleton state
// ────────────────────────────────────────────────────────────────────────

const RECONNECT_INITIAL_MS = 500;
const RECONNECT_MAX_MS = 30_000;

type EventListener = (event: ServerEvent) => void;
type StatusListener = (status: WsStatus) => void;

class WsClient {
  private socket: WebSocket | null = null;
  private status: WsStatus = 'idle';
  private reconnectDelay = RECONNECT_INITIAL_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private intentionalClose = false;
  private readonly eventListeners = new Set<EventListener>();
  private readonly statusListeners = new Set<StatusListener>();
  /// Outbound messages queued while the socket isn't open. Bounded so a
  /// runaway sender can't exhaust memory; entries beyond the cap are
  /// dropped (with a console warn) and the SPA will see them as
  /// "missing" if it wasn't tracking them via TanStack Query.
  private readonly outboundQueue: ClientEvent[] = [];
  private readonly OUTBOUND_QUEUE_CAP = 100;

  /** Open the socket if it isn't already. Idempotent — safe to call from N components. */
  connect(): void {
    if (typeof window === 'undefined') return; // SSR no-op
    if (this.socket && (this.status === 'open' || this.status === 'connecting')) {
      return;
    }
    this.intentionalClose = false;
    this.openSocket();
  }

  /** Tear down for an explicit logout. After this, `connect()` is needed again to reopen. */
  disconnect(): void {
    this.intentionalClose = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      try {
        this.socket.close(1000, 'client disconnect');
      } catch {
        // ignore
      }
      this.socket = null;
    }
    this.setStatus('closed');
  }

  /** Send an event. Buffers if the socket isn't open yet (or is reconnecting). */
  send(event: ClientEvent): void {
    if (this.socket && this.status === 'open') {
      try {
        this.socket.send(JSON.stringify(event));
        return;
      } catch (e) {
        // The socket reported open but the underlying transport failed.
        // Queue the message and let the reconnect path retry.
        console.warn('[ws] send failed; queueing', e);
      }
    }
    if (this.outboundQueue.length >= this.OUTBOUND_QUEUE_CAP) {
      console.warn('[ws] outbound queue full; dropping oldest');
      this.outboundQueue.shift();
    }
    this.outboundQueue.push(event);
    // Make sure we're trying to reopen — caller may not have called connect()
    // yet (e.g. send() racing with first mount).
    if (!this.socket && !this.intentionalClose) this.connect();
  }

  /** Subscribe to all server events. Returns an unsubscribe handle. */
  on(listener: EventListener): () => void {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  }

  /** Subscribe to status transitions. The listener fires immediately with the current status. */
  onStatus(listener: StatusListener): () => void {
    this.statusListeners.add(listener);
    listener(this.status);
    return () => this.statusListeners.delete(listener);
  }

  getStatus(): WsStatus {
    return this.status;
  }

  // ────────────────────────────────────────────────────────────────────
  // Internals
  // ────────────────────────────────────────────────────────────────────

  private openSocket(): void {
    this.setStatus('connecting');
    let socket: WebSocket;
    try {
      socket = new WebSocket(getWebSocketUrl());
    } catch (e) {
      console.warn('[ws] construct failed; will retry', e);
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;

    socket.addEventListener('open', () => {
      this.setStatus('open');
      this.reconnectDelay = RECONNECT_INITIAL_MS;
      // Flush anything that built up while we were down.
      while (this.outboundQueue.length > 0) {
        const next = this.outboundQueue.shift()!;
        try {
          socket.send(JSON.stringify(next));
        } catch (e) {
          console.warn('[ws] flush send failed; re-queueing', e);
          this.outboundQueue.unshift(next);
          break;
        }
      }
    });

    socket.addEventListener('message', (ev) => {
      let parsed: ServerEvent;
      try {
        parsed = JSON.parse(ev.data) as ServerEvent;
      } catch (e) {
        console.warn('[ws] received non-JSON frame; dropping', e);
        return;
      }
      // Snapshot via Array.from — iterating a Set directly trips
      // --downlevelIteration on the project's TS config.
      Array.from(this.eventListeners).forEach((l) => {
        try {
          l(parsed);
        } catch (le) {
          console.warn('[ws] listener threw', le);
        }
      });
    });

    socket.addEventListener('close', () => {
      this.socket = null;
      this.setStatus('closed');
      if (!this.intentionalClose) this.scheduleReconnect();
    });

    socket.addEventListener('error', () => {
      // We rely on the subsequent 'close' for reconnect scheduling — the
      // browser fires both for a failed handshake.
    });
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) return;
    const delay = this.reconnectDelay;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      // Exponential back-off with hard ceiling. Doubled on each attempt;
      // a successful open resets it to RECONNECT_INITIAL_MS.
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
      this.openSocket();
    }, delay);
  }

  private setStatus(next: WsStatus): void {
    if (this.status === next) return;
    this.status = next;
    Array.from(this.statusListeners).forEach((l) => {
      try {
        l(next);
      } catch (e) {
        console.warn('[ws] status listener threw', e);
      }
    });
  }
}

export const wsClient = new WsClient();
