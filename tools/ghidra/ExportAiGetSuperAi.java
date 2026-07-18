// Emits the GOG ai.dll GetSuperAI export to recover the live singleton
// boundary for read-only handler-input capture.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiGetSuperAi extends GhidraScript {
    private static final long ADDRESS = 0x1000f780L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionAt(address);
        println("===== AI GetSuperAI =====");
        if (function == null) { println("missing"); return; }
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
