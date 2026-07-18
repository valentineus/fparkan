// Emits record construction, equality, refresh and insertion helpers called by
// the corpus-reachable AI VM Handler(2) scheduler boundary.
// Run through Ghidra headless analysis; the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler2SchedulerHelpers extends GhidraScript {
    private static final long[] ADDRESSES = {
        0x10004e50L, 0x10004c50L, 0x10005070L, 0x100073e0L
    };

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("===== AI Handler(2) scheduler helper " + address + " =====");
            if (function == null) { println("missing"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
