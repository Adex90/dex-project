/**
 * Saving a file the page built (a document's JSON, the accounting CSV): a
 * Blob behind a temporary link, clicked once. The object URL is revoked
 * after the click has been handled; the download keeps its own reference.
 */
export function saveFile(name: string, content: string, type: string): void {
  const url = URL.createObjectURL(new Blob([content], { type }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}
