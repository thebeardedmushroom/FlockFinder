/// <reference lib="webworker" />
// Clustering off the main thread. Messages are processed strictly in order, so a query sent
// after a build always sees the new index; the main thread tags each request with the build
// generation and drops answers that belong to an older one.
import { ClusterEngine, type BBoxTuple, type EngineFilter, type EnginePoints, type EngineUser } from "../lib/clusterEngine";

export type WorkerRequest =
  | { type: "data"; points: EnginePoints; users: EngineUser[] }
  | { type: "build"; gen: number; filter: EngineFilter }
  | { type: "query"; seq: number; gen: number; bbox: BBoxTuple; level: number; prev: { level: number; ids: Float64Array } | null }
  | { type: "counts"; seq: number; gen: number; bbox: BBoxTuple }
  | { type: "expand"; seq: number; gen: number; id: number };

const engine = new ClusterEngine();
const scope = self as unknown as DedicatedWorkerGlobalScope;

scope.onmessage = (e: MessageEvent<WorkerRequest>) => {
  const m = e.data;
  try {
    switch (m.type) {
      case "data":
        engine.setData(m.points, m.users);
        break;
      case "build": {
        // Hex bins first: the wide band can draw while the cluster index is still building.
        const h = engine.buildHex(m.filter);
        const transfer: Transferable[] = [];
        for (const l of h.hex) transfer.push(l.q.buffer, l.r.buffer, l.count.buffer, l.users.buffer);
        scope.postMessage({ type: "hex", gen: m.gen, result: h }, transfer);
        scope.postMessage({ type: "built", gen: m.gen, result: engine.buildIndex() });
        break;
      }
      case "query": {
        const r = engine.query(m.bbox, m.level, m.prev);
        const transfer: Transferable[] = [r.ids.buffer, r.x.buffer, r.y.buffer, r.count.buffer, r.users.buffer, r.leaf.buffer];
        if (r.anc) transfer.push(r.anc.buffer);
        scope.postMessage({ type: "reply", seq: m.seq, gen: m.gen, result: r }, transfer);
        break;
      }
      case "counts":
        scope.postMessage({ type: "reply", seq: m.seq, gen: m.gen, result: engine.counts(m.bbox) });
        break;
      case "expand":
        scope.postMessage({ type: "reply", seq: m.seq, gen: m.gen, result: engine.expand(m.id) });
        break;
    }
  } catch (err) {
    const seq = "seq" in m ? m.seq : -1;
    scope.postMessage({ type: "error", seq, gen: "gen" in m ? m.gen : -1, message: String(err) });
  }
};
