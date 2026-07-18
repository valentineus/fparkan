// Emits the direct post-insertion dispatcher reached by the corpus-reachable
// AI VM Handler(2) scheduler boundary. Run headless; the original PE is read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler2Dispatch extends GhidraScript {
    private static final long[] ADDRESSES = {
        0x1000f920L, 0x10004be0L, 0x10004d00L, 0x10004db0L
    };

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("===== AI Handler(2) post-insertion helper " + address + " =====");
            if (function == null) { println("missing"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
