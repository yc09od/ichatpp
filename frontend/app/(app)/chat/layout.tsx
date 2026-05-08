// /chat layout — two-pane on lg+, drawer on mobile (TODO [39] / [40]).
//
// Implementation notes:
//  - The active friend id is derived from the pathname rather than a
//    state hook so deep-links (`/chat/<uuid>`) highlight the right
//    contact on first paint.
//  - The mobile drawer is a simple uncontrolled <details> equivalent —
//    open state lives here, hidden on lg+ via Tailwind's responsive
//    classes.

'use client';

import { useState, type ReactNode } from 'react';
import { usePathname } from 'next/navigation';
import { ContactList } from '@/components/ContactList';

function activeFriendIdFromPath(path: string): string | undefined {
  // Path shape: `/chat` or `/chat/<uuid>`
  const m = path.match(/^\/chat\/([^/]+)/);
  return m ? m[1] : undefined;
}

export default function ChatLayout({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  const activeFriendId = activeFriendIdFromPath(pathname);
  const [drawerOpen, setDrawerOpen] = useState(false);

  return (
    <div className="flex flex-1 min-h-0 relative">
      {/* Desktop sidebar — hidden on < lg */}
      <div className="hidden lg:block w-72 shrink-0">
        <ContactList activeFriendId={activeFriendId} />
      </div>

      {/* Mobile drawer trigger */}
      <button
        type="button"
        onClick={() => setDrawerOpen(true)}
        className="lg:hidden absolute top-2 left-2 z-10 rounded-md bg-white dark:bg-slate-900 border border-slate-200 dark:border-slate-800 shadow px-3 py-1.5 text-sm"
      >
        好友
      </button>

      {/* Mobile drawer */}
      {drawerOpen && (
        <div className="lg:hidden fixed inset-0 z-40 flex">
          <div
            className="absolute inset-0 bg-slate-900/40"
            onClick={() => setDrawerOpen(false)}
          />
          <div className="relative w-72 max-w-[80%] h-full">
            <ContactList
              activeFriendId={activeFriendId}
              onSelect={() => setDrawerOpen(false)}
            />
          </div>
        </div>
      )}

      <div className="flex-1 min-w-0 min-h-0 flex flex-col">{children}</div>
    </div>
  );
}
