// /chat — empty state shown when no friend is selected.

export default function ChatIndexPage() {
  return (
    <div className="flex-1 flex items-center justify-center text-slate-500">
      <div className="text-center">
        <p className="text-lg font-medium">选择一个好友开始聊天</p>
        <p className="text-sm mt-1">在左侧（或顶部&ldquo;好友&rdquo;按钮）打开联系人列表</p>
      </div>
    </div>
  );
}
