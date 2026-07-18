// Emits the immediate .scr package reader called by the AI script loader.
// Run through Ghidra headless analysis; the original PE is never modified.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiScriptPackageReader extends GhidraScript {
    private static final long ADDRESS = 0x10011B20L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionAt(address);
        println("===== AI .scr package reader =====");
        if (function == null) { println("missing"); return; }
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
