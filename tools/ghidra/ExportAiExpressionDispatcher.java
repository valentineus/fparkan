// Emits the decompiled AI expression evaluator containing the recovered
// tag 1..5 dispatch. Run through Ghidra headless analysis only; it never
// mutates the original PE image.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAiExpressionDispatcher extends GhidraScript {
    private static final long ADDRESS = 0x10005180L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionContaining(address);
        println("===== AI expression dispatcher =====");
        if (function == null) { println("missing"); return; }
        println("entry=" + function.getEntryPoint());
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
