// WebSocket client for ichatpp.
//
// This file currently only exports the protocol types and a URL helper.
// The full singleton client (auto-reconnect, EventEmitter, React Context
// binding) is implemented in TODO [41]; see ARCHITECTURE.md §4.7.
//
// Authentication: the browser automatically sends the httpOnly `access_token`
// cookie during the WebSocket upgrade — there is no in-band token to attach.

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
      from_user_id: string;
      content: string;
      emoji_ids: string[];
      created_at: string;
    }
  | { type: 'error'; code: string; message: string };

export function getWebSocketUrl(): string {
  return process.env.NEXT_PUBLIC_WS_URL ?? 'ws://localhost:8080/api/ws';
}

// Implemented in TODO [41]:
export const wsClient = {
  connect(): never {
    throw new Error('WebSocket client not implemented yet — see TODO [41].');
  },
} as const;
