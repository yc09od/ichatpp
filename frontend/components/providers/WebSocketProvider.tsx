// React Context wrapper around the WS singleton (TODO [41]).
//
// The singleton lives in `lib/websocket.ts` and works without React;
// this provider is mostly a "open-on-mount, close-on-unmount" lifecycle
// hook plus a hook (`useWebSocketEvent`) that wires individual
// listeners with the right cleanup.

'use client';

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import {
  type ServerEvent,
  type WsStatus,
  type ClientEvent,
  wsClient,
} from '@/lib/websocket';

interface WebSocketContextValue {
  status: WsStatus;
  send: (event: ClientEvent) => void;
}

const WebSocketContext = createContext<WebSocketContextValue | null>(null);

export function WebSocketProvider({ children }: { children: React.ReactNode }) {
  const [status, setStatus] = useState<WsStatus>(wsClient.getStatus());

  useEffect(() => {
    // Subscribe before connect so the initial 'connecting' transition is
    // observed.
    const unsub = wsClient.onStatus(setStatus);
    wsClient.connect();
    return () => {
      unsub();
      // We deliberately do NOT call disconnect() here — other tabs in
      // the same React tree may have lingering subscriptions and the
      // singleton is shared across navigations within the SPA.
    };
  }, []);

  const send = useCallback((event: ClientEvent) => wsClient.send(event), []);

  return (
    <WebSocketContext.Provider value={{ status, send }}>{children}</WebSocketContext.Provider>
  );
}

export function useWebSocketStatus(): WsStatus {
  const ctx = useContext(WebSocketContext);
  if (!ctx) throw new Error('useWebSocketStatus must be used inside <WebSocketProvider>');
  return ctx.status;
}

export function useSendWebSocket() {
  const ctx = useContext(WebSocketContext);
  if (!ctx) throw new Error('useSendWebSocket must be used inside <WebSocketProvider>');
  return ctx.send;
}

/**
 * Subscribe a callback to all server events for the lifetime of the component.
 * The callback ref is read live so closures over current state stay fresh
 * without forcing a resubscribe on every render.
 */
export function useWebSocketEvent(callback: (event: ServerEvent) => void): void {
  const ref = useRef(callback);
  ref.current = callback;
  useEffect(() => {
    const unsub = wsClient.on((ev) => ref.current(ev));
    return unsub;
  }, []);
}
