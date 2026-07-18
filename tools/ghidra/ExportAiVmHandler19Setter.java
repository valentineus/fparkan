// Emits Handler(19)'s common x87 varset setter. Run headless; the original PE
// remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiVmHandler19Setter extends GhidraScript {
    private static final long ADDRESS = 0x10013770L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionAt(address);
        println("===== AI VM Handler(19) setter =====");
        if (function == null) { println("missing"); return; }
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
