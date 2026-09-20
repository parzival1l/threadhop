export function sessionLabel(sessionId: string, title?: string): string {
  return title?.trim() || sessionId;
}
