export let globalDragItem: { type: "local" | "mtp"; data: any } | null = null;

export function setGlobalDragItem(item: { type: "local" | "mtp"; data: any } | null) {
  globalDragItem = item;
}
