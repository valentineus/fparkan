// Emits the state-transition helpers selected by Handler(8). Run headless;
// the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler8Transitions extends GhidraScript {
    private static final long[] ADDRESSES = {0x10005010L, 0x10005040L};

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("===== AI Handler(8) transition " + address + " =====");
            if (function == null) { println("missing"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
