// Emits Handler(30), the most frequent non-sentinel selector in the GOG
// compiled-script corpus. Run headless; the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler30 extends GhidraScript {
    private static final long ADDRESS = 0x1000c266L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionAt(address);
        println("===== AI VM Handler(30) =====");
        if (function == null) { println("missing"); return; }
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
