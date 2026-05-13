import { JobStateTag, JobView, QueueGroupView } from "../types";

interface Props {
  jobs: JobView[];
  groups: Map<number, QueueGroupView>;
  onCancel: (id: number) => void;
}

interface QueueRow {
  key: string;
  label: string;
  direction: "download" | "upload";
  jobs: JobView[];
  totalFiles: number;
  stateTag: JobStateTag;
  stateLabel: string;
  sent: number;
  total: number;
  reason?: string;
  startedAt: number;
  /** BulkUpload-specific progress (files-done / total-files / current
   * file) sourced from `bulkProgress` events. Small files often skip
   * libmtp's byte-level progress callback entirely, so this is the
   * only visible signal of forward motion for directory uploads. */
  bulkFilesDone?: number;
  bulkTotalFiles?: number;
  bulkCurrentFile?: string;
}

export function QueuePanel({ jobs, groups, onCancel }: Props) {
  const rows = buildRows(jobs, groups);
  const totalFiles = jobs.length;
  const remainingFiles = jobs.filter((job) => !isTerminal(job.state.tag)).length;

  return (
    <div className="queue">
      <div className="queue-header">
        전송 큐
        <span className="count">
          남은 {remainingFiles} / 전체 {totalFiles}
        </span>
      </div>
      <div className="queue-list">
        {rows.length === 0 && (
          <div className="queue-empty">대기 중인 전송 작업이 없습니다.</div>
        )}
        {rows.map((row) => {
          // For bulk uploads (single job that internally walks many
          // files), prefer the file-count progress from `bulkProgress`
          // events because small files often skip libmtp's byte-level
          // callback and `row.sent` barely moves.
          const useBulkProgress =
            row.bulkTotalFiles !== undefined && row.bulkTotalFiles > 0;
          const progressPct = useBulkProgress
            ? Math.min(
                100,
                Math.floor(((row.bulkFilesDone ?? 0) / (row.bulkTotalFiles ?? 1)) * 100),
              )
            : row.total > 0
              ? Math.min(100, Math.floor((row.sent / row.total) * 100))
              : row.stateTag === "completed"
                ? 100
                : 0;
          const completedFiles = useBulkProgress
            ? row.bulkFilesDone ?? 0
            : row.jobs.filter((job) => job.state.tag === "completed").length;
          const totalFilesShown = useBulkProgress
            ? row.bulkTotalFiles ?? row.totalFiles
            : row.totalFiles;
          const active = row.jobs.filter((job) => !isTerminal(job.state.tag));

          return (
            <div className="job" key={row.key}>
              <div className="top">
                <div className={`dir-badge ${row.direction === "download" ? "dl" : "ul"}`}>
                  {row.direction === "download" ? "↓" : "↑"}
                </div>
                <div className="name" title={row.label}>
                  {row.label}
                </div>
                <div className={`state ${row.stateTag}`}>{row.stateLabel}</div>
              </div>
              <div className="meta">
                {totalFilesShown > 1 && (
                  <span>
                    {completedFiles}/{totalFilesShown} 파일 완료
                  </span>
                )}
                {row.stateTag === "transferring" && (row.total > 0 || useBulkProgress) && (
                  <span>
                    {totalFilesShown > 1 ? " · " : ""}
                    {progressPct}%
                    {row.total > 0 && (
                      <> ({formatBytes(row.sent)} / {formatBytes(row.total)})</>
                    )}
                  </span>
                )}
                {row.stateTag === "transferring" && row.bulkCurrentFile && (
                  <span className="current-file" title={row.bulkCurrentFile}>
                    {" · "}현재: {row.bulkCurrentFile}
                  </span>
                )}
                {row.stateTag === "completed" && row.totalFiles === 1 && row.jobs[0].state.bytes !== undefined && (
                  <span>{formatBytes(row.jobs[0].state.bytes)} 전송됨</span>
                )}
              </div>
              {["queued", "validating", "transferring", "completed"].includes(row.stateTag) && (
                <div className="bar">
                  <div style={{ width: `${progressPct}%` }} />
                </div>
              )}
              {row.reason && <div className="err">{row.reason}</div>}
              {active.length > 0 && (
                <div className="actions">
                  <button
                    onClick={() => {
                      for (const job of active) onCancel(job.id);
                    }}
                    className="danger"
                  >
                    취소
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function buildRows(jobs: JobView[], groups: Map<number, QueueGroupView>): QueueRow[] {
  const buckets = new Map<string, { group?: QueueGroupView; jobs: JobView[] }>();

  for (const job of jobs) {
    const group = groups.get(job.id);
    const key = group?.id ?? `job-${job.id}`;
    const bucket = buckets.get(key) ?? { group, jobs: [] };
    bucket.jobs.push(job);
    buckets.set(key, bucket);
  }

  return Array.from(buckets.entries())
    .map(([key, bucket]) => makeRow(key, bucket.jobs, bucket.group))
    .sort((a, b) => b.startedAt - a.startedAt);
}

function makeRow(key: string, jobs: JobView[], group?: QueueGroupView): QueueRow {
  const first = jobs[0];
  const bulk = jobs.find((j) => j.kind.kind === "bulkUpload");
  const totalFiles = group?.totalFiles ?? bulk?.totalFiles ?? jobs.length;
  const label = group
    ? `${group.label} (${totalFiles}개 파일)`
    : bulk && bulk.totalFiles
      ? `${first.kind.name} (${bulk.totalFiles}개 파일)`
      : first.kind.name;
  const sent = jobs.reduce((sum, job) => sum + job.sent, 0);
  const total = jobs.reduce((sum, job) => sum + job.total, 0);
  const stateTag = deriveState(jobs);

  return {
    key,
    label,
    direction: first.kind.kind === "bulkUpload" ? "upload" : first.kind.kind,
    jobs,
    totalFiles,
    stateTag,
    stateLabel: stateLabel(stateTag),
    sent,
    total,
    reason: jobs.find((job) => job.state.reason)?.state.reason,
    startedAt: Math.max(...jobs.map((job) => job.startedAt)),
    bulkFilesDone: bulk?.filesDone,
    bulkTotalFiles: bulk?.totalFiles,
    bulkCurrentFile: bulk?.currentFile,
  };
}

function deriveState(jobs: JobView[]): JobStateTag {
  const tags = jobs.map((job) => job.state.tag);
  if (tags.includes("transferring")) return "transferring";
  if (tags.includes("cancelling")) return "cancelling";
  if (tags.includes("validating")) return "validating";
  if (tags.includes("queued")) return "queued";
  if (tags.includes("failed")) return "failed";
  if (tags.every((tag) => tag === "cancelled")) return "cancelled";
  if (tags.every((tag) => tag === "skipped")) return "skipped";
  return "completed";
}

function stateLabel(tag: JobStateTag): string {
  switch (tag) {
    case "queued":
      return "대기 중";
    case "validating":
      return "확인 중";
    case "transferring":
      return "전송 중";
    case "cancelling":
      return "취소 중";
    case "completed":
      return "완료";
    case "failed":
      return "실패";
    case "cancelled":
      return "취소됨";
    case "skipped":
      return "건너뜀";
  }
}

function isTerminal(tag: JobStateTag): boolean {
  return ["completed", "failed", "cancelled", "skipped"].includes(tag);
}

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}
