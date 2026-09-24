// Main-thread side of the clustering worker: typed request/response over postMessage.
import type {
  BBoxTuple,
  EngineFilter,
  EnginePoints,
  EngineUser,
  HexResult,
  IndexResult,
  QueryResult,
  ViewCounts,
} from "../lib/clusterEngine";
import type { WorkerRequest } from "../workers/cluster.worker";

interface Reply<T> {
  gen: number;
  result: T;
}

type Pending = { resolve: (v: Reply<unknown>) => void; reject: (e: unknown) => void };

type Req =
  | Omit<Extract<WorkerRequest, { type: "query" }>, "seq" | "gen">
  | Omit<Extract<WorkerRequest, { type: "counts" }>, "seq" | "gen">
  | Omit<Extract<WorkerRequest, { type: "expand" }>, "seq" | "gen">;

export class ClusterClient {
  private worker: Worker;
  private seq = 0;
  private pending = new Map<number, Pending>();
  /** Generation of the most recently requested build. */
  gen = 0;
  /** A build's filtered set and hex bins are ready (arrives before onBuilt). */
  onHex: ((gen: number, result: HexResult) => void) | null = null;
  /** A build's cluster index is ready; queries now answer for this generation. */
  onBuilt: ((gen: number, result: IndexResult) => void) | null = null;

  constructor() {
    this.worker = new Worker(new URL("../workers/cluster.worker.ts", import.meta.url), { type: "module" });
    this.worker.onmessage = (e: MessageEvent) => {
      const m = e.data;
      if (m.type === "hex") {
        if (m.gen === this.gen) this.onHex?.(m.gen, m.result as HexResult);
        return;
      }
      if (m.type === "built") {
        if (m.gen === this.gen) this.onBuilt?.(m.gen, m.result as IndexResult);
        return;
      }
      const p = this.pending.get(m.seq);
      if (!p) {
        if (m.type === "error") console.error("cluster worker:", m.message);
        return;
      }
      this.pending.delete(m.seq);
      if (m.type === "error") p.reject(new Error(m.message));
      else p.resolve({ gen: m.gen, result: m.result });
    };
    this.worker.onerror = (e) => console.error("cluster worker failed:", e.message);
  }

  private post(msg: WorkerRequest): void {
    this.worker.postMessage(msg);
  }

  setData(points: EnginePoints, users: EngineUser[]): void {
    this.post({ type: "data", points, users });
  }

  /** Start a rebuild; returns its generation. Answers to older generations are dropped by callers. */
  build(filter: EngineFilter): number {
    this.gen += 1;
    this.post({ type: "build", gen: this.gen, filter });
    return this.gen;
  }

  private request<T>(req: Req): Promise<Reply<T>> {
    const seq = ++this.seq;
    return new Promise((resolve, reject) => {
      this.pending.set(seq, { resolve: resolve as (v: Reply<unknown>) => void, reject });
      this.post({ ...req, seq, gen: this.gen } as WorkerRequest);
    });
  }

  query(bbox: BBoxTuple, level: number, prev: { level: number; ids: Float64Array } | null): Promise<Reply<QueryResult>> {
    return this.request({ type: "query", bbox, level, prev });
  }

  counts(bbox: BBoxTuple): Promise<Reply<ViewCounts>> {
    return this.request({ type: "counts", bbox });
  }

  expand(id: number): Promise<Reply<{ bounds: BBoxTuple; zoom: number } | null>> {
    return this.request({ type: "expand", id });
  }

  destroy(): void {
    this.worker.terminate();
    for (const p of this.pending.values()) p.reject(new Error("cluster worker stopped"));
    this.pending.clear();
  }
}
