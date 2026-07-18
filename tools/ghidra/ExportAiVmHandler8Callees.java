// Emits the two non-trivial local callees reached by Handler(8). Run headless;
// the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler8Callees extends GhidraScript {
    private static final long[] ADDRESSES = {0x10002e90L, 0x10005710L};

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("===== AI Handler(8) callee " + address + " =====");
            if (function == null) { println("missing"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
