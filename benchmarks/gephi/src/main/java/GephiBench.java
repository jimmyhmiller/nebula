import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import org.gephi.project.api.ProjectController;
import org.gephi.graph.api.GraphController;
import org.gephi.graph.api.GraphModel;
import org.gephi.graph.api.Node;
import org.gephi.layout.plugin.forceAtlas2.ForceAtlas2;
import org.openide.util.Lookup;

/** Shared-fixture ForceAtlas2 throughput; no UI, rendering, or import in timed steps. */
public class GephiBench {
    public static void main(String[] args) throws Exception {
        int warmup = Integer.parseInt(args[1]);
        int steps = Integer.parseInt(args[2]);
        int threads = Integer.parseInt(args[3]);
        if (warmup < 0 || steps < 1 || threads < 1) throw new IllegalArgumentException("Invalid run lengths/threads");
        long start = System.nanoTime();
        ByteBuffer data = ByteBuffer.wrap(Files.readAllBytes(Path.of(args[0]))).order(ByteOrder.LITTLE_ENDIAN);
        if (data.getLong() != 0x3148434e45424eL) throw new IllegalArgumentException("Invalid fixture");
        int n = data.getInt();
        long m = data.getLong();
        Lookup.getDefault().lookup(ProjectController.class).newProject();
        GraphModel model = Lookup.getDefault().lookup(GraphController.class).getGraphModel();
        var graph = model.getUndirectedGraph();
        Node[] nodes = new Node[n];
        for (int i = 0; i < n; i++) {
            Node node = model.factory().newNode(Integer.toString(i));
            node.setX(data.getFloat()); node.setY(data.getFloat()); node.setSize(1f);
            graph.addNode(node); nodes[i] = node;
        }
        for (long i = 0; i < m; i++) {
            int a = data.getInt(), b = data.getInt();
            if (!graph.addEdge(model.factory().newEdge(nodes[a], nodes[b], false)))
                throw new IllegalArgumentException("Edge rejected: " + a + " " + b);
        }
        if (data.hasRemaining() || graph.getEdgeCount() != m) throw new IllegalArgumentException("Fixture/count mismatch");
        double loadMs = (System.nanoTime() - start) / 1e6;
        ForceAtlas2 layout = new ForceAtlas2(null);
        layout.setGraphModel(model);
        layout.setBarnesHutOptimize(true);
        layout.setBarnesHutTheta(1.2);
        layout.setThreadsCount(threads);
        layout.initAlgo();
        for (int i = 0; i < warmup; i++) layout.goAlgo();
        start = System.nanoTime();
        for (int i = 0; i < steps; i++) {
            layout.goAlgo();
            if (args.length == 5 && (i + 1) % 100 == 0) System.err.println("ForceAtlas2: " + (warmup + i + 1) + " iterations");
        }
        double elapsedMs = (System.nanoTime() - start) / 1e6;
        layout.endAlgo();
        for (Node node : nodes) {
            if (!Float.isFinite(node.x()) || !Float.isFinite(node.y())) throw new IllegalStateException("Nonfinite layout");
        }
        if (args.length == 5) {
            ByteBuffer positions = ByteBuffer.allocate(Math.multiplyExact(n, 8)).order(ByteOrder.LITTLE_ENDIAN);
            for (Node node : nodes) { positions.putFloat(node.x()); positions.putFloat(node.y()); }
            Files.write(Path.of(args[4]), positions.array());
        } else if (args.length != 4) {
            throw new IllegalArgumentException("GephiBench FIXTURE WARMUP STEPS THREADS [POSITIONS]");
        }
        System.out.printf(java.util.Locale.ROOT,
            "{\"engine\":\"gephi-0.11.3-forceatlas2\",\"nodes\":%d,\"edges\":%d,\"threads\":%d,\"warmup\":%d,\"steps\":%d,\"load_ms\":%.6f,\"step_ms\":%.6f}%n",
            n, m, threads, warmup, steps, loadMs, elapsedMs / steps);
    }
}
