'use client';

import { useParams } from 'next/navigation';
import { ChatWindow } from '@/components/ChatWindow';

export default function ChatFriendPage() {
  const params = useParams<{ friend_id: string }>();
  return <ChatWindow friendId={params.friend_id} />;
}
