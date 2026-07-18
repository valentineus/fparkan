// Emits the two direct callees recovered from AI VM Handler(1).
// Run through Ghidra headless analysis; the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler1Callees extends GhidraScript {
    private static final long[] ADDRESSES = { 0x10002d30L, 0x10013190L };

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("===== AI VM Handler(1) callee " + address + " =====");
            if (function == null) { println("missing"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
